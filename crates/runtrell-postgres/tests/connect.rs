// SPDX-License-Identifier: Apache-2.0
//! `PgStore::connect` against real PostgreSQL: an owner role initializes the
//! runtime, then a dedicated LOGIN role with no CREATE/ALTER/ownership attaches.
#[allow(dead_code)]
mod support;
use runtrell::*;
use runtrell_postgres::PgStore;
use serde_json::Value;
use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};

const SNAPSHOT: &str = "SELECT jsonb_build_object(\
    'config', (SELECT jsonb_agg(to_jsonb(c)) FROM {s}.schema_config c),\
    'config_xmin', (SELECT string_agg(xmin::text, ',') FROM {s}.schema_config),\
    'slots', (SELECT md5(string_agg(scope||':'||ordinal||':'||coalesce(holder,''), ',' ORDER BY scope, ordinal)) FROM {s}.capacity_slots),\
    'slot_count', (SELECT count(*) FROM {s}.capacity_slots),\
    'slot_xmin', (SELECT max(xmin::text::bigint) FROM {s}.capacity_slots),\
    'objects', (SELECT jsonb_agg(c.relname ORDER BY c.relname) FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='{n}'),\
    'columns', (SELECT count(*) FROM pg_attribute a JOIN pg_class c ON c.oid=a.attrelid JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='{n}' AND a.attnum>0 AND NOT a.attisdropped),\
    'schema_acl', (SELECT nspacl::text FROM pg_namespace WHERE nspname='{n}'))";

struct Env {
    admin: PgPool,
    options: PgConnectOptions,
    schema: String,
    /// Prefix of every role this test creates; cleanup drops them by prefix.
    role_prefix: String,
}
impl Env {
    async fn new() -> Self {
        let url = std::env::var("RUNTRELL_TEST_DATABASE_URL").expect("dedicated database required");
        let options: PgConnectOptions = url.parse().unwrap();
        let admin = PgPoolOptions::new()
            .max_connections(4)
            .connect_with(options.clone())
            .await
            .unwrap();
        Self {
            admin,
            options,
            schema: format!("connect_{}", uuid::Uuid::new_v4().simple()),
            role_prefix: format!(
                "rt_conn_{}_",
                &uuid::Uuid::new_v4().simple().to_string()[..12]
            ),
        }
    }
    async fn sql(&self, sql: &str) {
        sqlx::query(sql).execute(&self.admin).await.unwrap();
    }
    /// Role with LOGIN only: no CREATE on the schema or database, owns nothing.
    async fn login_role(&mut self) -> (String, PgPool) {
        let name = format!(
            "{}{}",
            self.role_prefix,
            &uuid::Uuid::new_v4().simple().to_string()[..12]
        );
        let password = uuid::Uuid::new_v4().simple().to_string();
        self.sql(&format!(
            "CREATE ROLE {name} LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT PASSWORD '{password}'"
        ))
        .await;
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(self.options.clone().username(&name).password(&password))
            .await
            .unwrap();
        (name, pool)
    }
    async fn snapshot(&self) -> Value {
        let sql = SNAPSHOT
            .replace("{s}", &format!("\"{}\"", self.schema))
            .replace("{n}", &self.schema);
        sqlx::query_scalar(&sql)
            .fetch_one(&self.admin)
            .await
            .unwrap()
    }
    async fn cleanup(&self) {
        let _ = sqlx::query(&format!(
            "DROP SCHEMA IF EXISTS \"{}\" CASCADE",
            self.schema
        ))
        .execute(&self.admin)
        .await;
        let roles: Vec<String> = sqlx::query_scalar(
            "SELECT rolname::text FROM pg_roles WHERE starts_with(rolname::text, $1)",
        )
        .bind(&self.role_prefix)
        .fetch_all(&self.admin)
        .await
        .unwrap_or_default();
        for role in &roles {
            let _ = sqlx::query(&format!("DROP OWNED BY {role}"))
                .execute(&self.admin)
                .await;
            let _ = sqlx::query(&format!("DROP ROLE IF EXISTS {role}"))
                .execute(&self.admin)
                .await;
        }
    }
}
fn capacity(queue: usize) -> Capacity {
    Capacity {
        queue,
        ..Capacity::default()
    }
}
/// Runs `body`, always removes the schema and roles, then re-raises a failure.
fn run<F, Fut>(body: F)
where
    F: FnOnce(Env) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Env> + Send + 'static,
{
    support::block_on(async move {
        let env = Env::new().await;
        let (admin, options, schema, role_prefix) = (
            env.admin.clone(),
            env.options.clone(),
            env.schema.clone(),
            env.role_prefix.clone(),
        );
        let outcome = tokio::spawn(body(env)).await;
        let cleaner = Env {
            admin,
            options,
            schema,
            role_prefix,
        };
        cleaner.cleanup().await;
        outcome.unwrap();
    });
}

#[test]
fn least_privilege_role_connects_without_changing_the_runtime() {
    run(|mut env| async move {
        // Owner role initializes: creates the schema, runs DDL, seeds config and permits.
        let owner = PgStore::initialize(env.admin.clone(), &env.schema, capacity(7))
            .await
            .unwrap();
        let account = owner
            .create_account("t", Amount::new(1_000, 1_000), None)
            .await
            .unwrap();
        let run = owner
            .start(RunSpec::new("t", "existing", account, "judge"))
            .await
            .unwrap();
        // Make the seed state observable: remove one scope's permits. Initialization
        // would restore them; connecting must not.
        env.sql(&format!(
            "DELETE FROM \"{}\".capacity_slots WHERE scope='q:global'",
            env.schema
        ))
        .await;

        // The least-privilege role: LOGIN, USAGE on the schema, SELECT on schema_config
        // and runs. Nothing else - no CREATE, no ALTER, no ownership, no write.
        let (role, pool) = env.login_role().await;
        let schema = env.schema.clone();
        env.sql(&format!("GRANT USAGE ON SCHEMA \"{schema}\" TO {role}"))
            .await;
        env.sql(&format!(
            "GRANT SELECT ON \"{schema}\".schema_config, \"{schema}\".runs TO {role}"
        ))
        .await;
        let (create, owns, superuser): (bool, bool, bool) = sqlx::query_as(
            "SELECT has_schema_privilege($1, $2, 'CREATE'), \
             (SELECT nspowner = r.oid FROM pg_namespace WHERE nspname = $2), r.rolsuper \
             FROM pg_roles r WHERE r.rolname = $1",
        )
        .bind(&role)
        .bind(&schema)
        .fetch_one(&env.admin)
        .await
        .unwrap();
        assert!(!create && !owns && !superuser);
        // The role really cannot do DDL or write config/capacity itself.
        for sql in [
            format!("CREATE TABLE \"{schema}\".extra(x int)"),
            format!("ALTER TABLE \"{schema}\".runs ADD COLUMN x int"),
            format!("UPDATE \"{schema}\".schema_config SET version = version"),
            format!(
                "INSERT INTO \"{schema}\".capacity_slots(scope, ordinal) VALUES ('q:global', 1)"
            ),
            format!("DELETE FROM \"{schema}\".capacity_slots"),
        ] {
            let error = sqlx::query(&sql).execute(&pool).await.unwrap_err();
            assert_eq!(
                error.as_database_error().and_then(|e| e.code()).as_deref(),
                Some("42501"),
                "{sql}"
            );
        }

        let before = env.snapshot().await;
        let queue_permits: i64 = sqlx::query_scalar(&format!(
            "SELECT count(*) FROM \"{}\".capacity_slots WHERE scope='q:global'",
            env.schema
        ))
        .fetch_one(&env.admin)
        .await
        .unwrap();
        assert_eq!(
            queue_permits, 0,
            "the removed permits are the observable seed state"
        );
        assert!(before["slot_count"].as_i64().unwrap() > 0);
        let store = PgStore::connect(pool.clone(), &env.schema, capacity(7))
            .await
            .unwrap();
        assert_eq!(
            env.snapshot().await,
            before,
            "connect changed schema_config, capacity or DDL"
        );
        // The connected store reads the existing runtime (SELECT on runs is granted).
        let record = store.get(&run).await.unwrap();
        assert_eq!(record.handle, run);

        // Contrast: the explicit initializer restores the removed permits.
        PgStore::initialize(env.admin.clone(), &env.schema, capacity(7))
            .await
            .unwrap();
        assert_ne!(env.snapshot().await, before);
        env
    });
}

#[test]
fn mismatch_and_missing_runtime_are_refused_without_writes() {
    run(|mut env| async move {
        let schema = env.schema.clone();
        let (role, pool) = env.login_role().await;
        let (blind_role, blind_pool) = env.login_role().await;

        // 1. Schema does not exist: NotFound, and nothing is created.
        assert_eq!(
            PgStore::connect(pool.clone(), &schema, capacity(7))
                .await
                .err(),
            Some(Error::NotFound)
        );
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_namespace WHERE nspname = $1)")
                .bind(&schema)
                .fetch_one(&env.admin)
                .await
                .unwrap();
        assert!(!exists, "connect must not create the schema");

        // 2. Schema exists but was never initialized: NotFound (no schema_config).
        env.sql(&format!("CREATE SCHEMA \"{schema}\"")).await;
        env.sql(&format!("GRANT USAGE ON SCHEMA \"{schema}\" TO {role}"))
            .await;
        assert_eq!(
            PgStore::connect(pool.clone(), &schema, capacity(7))
                .await
                .err(),
            Some(Error::NotFound)
        );
        let tables: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname = $1",
        )
        .bind(&schema)
        .fetch_one(&env.admin)
        .await
        .unwrap();
        assert_eq!(tables, 0, "connect must not run the migration");
        env.sql(&format!("DROP SCHEMA \"{schema}\"")).await;

        // 3. Initialized runtime; every refusal below leaves the snapshot untouched.
        PgStore::initialize(env.admin.clone(), &schema, capacity(7))
            .await
            .unwrap();
        env.sql(&format!("GRANT USAGE ON SCHEMA \"{schema}\" TO {role}"))
            .await;
        env.sql(&format!(
            "GRANT SELECT ON \"{schema}\".schema_config TO {role}"
        ))
        .await;
        let baseline = env.snapshot().await;

        // Different Capacity configuration.
        assert_eq!(
            PgStore::connect(pool.clone(), &schema, capacity(9))
                .await
                .err(),
            Some(Error::Conflict)
        );
        // Invalid identifier is rejected before any SQL.
        assert!(matches!(
            PgStore::connect(pool.clone(), "pg_catalog", capacity(7))
                .await
                .err(),
            Some(Error::Invalid(_))
        ));
        // No privileges on the schema at all.
        assert_eq!(
            PgStore::connect(blind_pool.clone(), &schema, capacity(7))
                .await
                .err(),
            Some(Error::PermissionDenied)
        );
        let _ = blind_role;
        assert_eq!(env.snapshot().await, baseline);

        // 4. Another schema version (the CHECK is dropped so a mismatch can exist).
        env.sql(&format!(
            "ALTER TABLE \"{schema}\".schema_config DROP CONSTRAINT schema_config_version_check"
        ))
        .await;
        env.sql(&format!(
            "UPDATE \"{schema}\".schema_config SET version = 5"
        ))
        .await;
        let versioned = env.snapshot().await;
        assert_eq!(
            PgStore::connect(pool.clone(), &schema, capacity(7))
                .await
                .err(),
            Some(Error::Unsupported("postgres schema".into()))
        );
        assert_eq!(env.snapshot().await, versioned);
        env.sql(&format!(
            "UPDATE \"{schema}\".schema_config SET version = 4"
        ))
        .await;

        // 5. A required table is absent.
        env.sql(&format!("DROP TABLE \"{schema}\".executions"))
            .await;
        let partial = env.snapshot().await;
        assert_eq!(
            PgStore::connect(pool.clone(), &schema, capacity(7))
                .await
                .err(),
            Some(Error::NotFound)
        );
        assert_eq!(env.snapshot().await, partial);

        // 6. The config row is absent.
        env.sql(&format!("DELETE FROM \"{schema}\".schema_config"))
            .await;
        let empty = env.snapshot().await;
        assert_eq!(
            PgStore::connect(pool.clone(), &schema, capacity(7))
                .await
                .err(),
            Some(Error::NotFound)
        );
        assert_eq!(env.snapshot().await, empty);
        env
    });
}

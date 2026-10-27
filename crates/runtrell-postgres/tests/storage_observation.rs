// SPDX-License-Identifier: Apache-2.0
use runtrell::{BudgetAuthority, Capacity, Error, RunSpec, RunStore};
use runtrell_postgres::{PgStore, storage_error};
use sqlx::{
    ConnectOptions, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::{
    filter::{LevelFilter, Targets},
    prelude::*,
};

#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<u8>>>);
impl<'a> MakeWriter<'a> for Log {
    type Writer = LogWriter;
    fn make_writer(&'a self) -> Self::Writer {
        LogWriter(self.0.clone())
    }
}
struct LogWriter(Arc<Mutex<Vec<u8>>>);
impl std::io::Write for LogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl Log {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

async fn pool() -> PgPool {
    let url = std::env::var("RUNTRELL_TEST_DATABASE_URL").expect("dedicated database required");
    PgPoolOptions::new()
        .max_connections(4)
        .connect_with(
            PgConnectOptions::from_str(&url)
                .unwrap()
                .disable_statement_logging(),
        )
        .await
        .unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn storage_canary_constraint_failure_logs_safe_fields() {
    let pool = pool().await;
    let schema = format!("storage_obs_{}", uuid::Uuid::new_v4().simple());
    let store = PgStore::open(pool.clone(), &schema, Capacity::default())
        .await
        .unwrap();
    let canary = "secret-canary-unique-46d8c3";
    let account = store
        .create_account("t", runtrell::Amount::new(100, 100), None)
        .await
        .unwrap();
    // A direct DB error proves that PostgreSQL includes the value in a failure.
    sqlx::query(&format!("CREATE TABLE {schema}.canary(v text UNIQUE)"))
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(&format!("INSERT INTO {schema}.canary(v) VALUES($1)"))
        .bind(canary)
        .execute(&pool)
        .await
        .unwrap();
    let raw = sqlx::query(&format!("INSERT INTO {schema}.canary(v) VALUES($1)"))
        .bind(canary)
        .execute(&pool)
        .await
        .unwrap_err();
    assert_eq!(
        raw.as_database_error().unwrap().code().as_deref(),
        Some("23505")
    );
    assert!(
        raw.as_database_error()
            .unwrap()
            .downcast_ref::<sqlx::postgres::PgDatabaseError>()
            .detail()
            .unwrap()
            .contains(canary)
    );
    let bad_type = sqlx::query("SELECT $1::text::numeric")
        .bind(canary)
        .execute(&pool)
        .await
        .unwrap_err();
    assert_eq!(
        bad_type.as_database_error().unwrap().code().as_deref(),
        Some("22P02")
    );
    assert!(
        bad_type
            .as_database_error()
            .unwrap()
            .message()
            .contains(canary)
    );
    sqlx::query(&format!(
        "CREATE TABLE {schema}.canary_check(v text CHECK (length(v) < 4))"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let bad_check = sqlx::query(&format!("INSERT INTO {schema}.canary_check(v) VALUES($1)"))
        .bind(canary)
        .execute(&pool)
        .await
        .unwrap_err();
    assert_eq!(
        bad_check.as_database_error().unwrap().code().as_deref(),
        Some("23514")
    );
    assert!(
        bad_check
            .as_database_error()
            .unwrap()
            .downcast_ref::<sqlx::postgres::PgDatabaseError>()
            .detail()
            .unwrap()
            .contains(canary)
    );
    sqlx::query(&format!("ALTER TABLE {schema}.budget_accounts ADD CONSTRAINT tenant_short CHECK (length(tenant) < 4)"))
        .execute(&pool).await.unwrap();
    let log = Log::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(log.clone())
        .with_ansi(false)
        .without_time()
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    assert_eq!(storage_error("canary.unique", raw), Error::Storage);
    assert_eq!(storage_error("canary.type", bad_type), Error::Storage);
    assert_eq!(storage_error("canary.check", bad_check), Error::Storage);
    assert_eq!(
        store
            .create_account(canary, runtrell::Amount::new(100, 100), None)
            .await,
        Err(Error::Storage)
    );
    // This controlled invalid input reaches a real PgStore SQL error conversion.
    store.fail_next_write();
    let result = store
        .start(RunSpec::new("t", canary, account, canary))
        .await;
    assert_eq!(result, Err(Error::Storage));
    let output = log.text();
    assert!(output.contains("operation=\"canary.unique\""), "{output}");
    assert!(output.contains("operation=\"canary.type\""), "{output}");
    assert!(output.contains("operation=\"canary.check\""), "{output}");
    assert!(output.contains("operation=\"PgStore::put\""), "{output}");
    assert!(output.contains("operation=\"PgStore::tx_in\""), "{output}");
    assert!(output.contains("sqlstate=\"23505\""), "{output}");
    assert!(output.contains("sqlstate=\"23514\""), "{output}");
    assert!(output.contains("sqlstate=\"22P02\""), "{output}");
    assert!(output.contains("sqlstate=\"22012\""), "{output}");
    assert!(output.contains("class="), "{output}");
    assert!(output.contains("correlation_id="), "{output}");
    assert!(!output.contains(canary), "canary leaked: {output}");
    assert!(!output.contains("SELECT 1/0"), "SQL leaked: {output}");
    for line in output.lines() {
        let fields = line.split("storage failure ").nth(1).unwrap();
        let fields: Vec<_> = fields.split_whitespace().collect();
        assert_eq!(fields.len(), 4, "unexpected field: {line}");
        assert!(fields[0].starts_with("operation=\""));
        assert!(fields[1].starts_with("sqlstate=\""));
        assert!(fields[2].starts_with("class=\""));
        let correlation = fields[3].strip_prefix("correlation_id=").unwrap();
        uuid::Uuid::parse_str(correlation).unwrap();
    }
}

#[test]
fn storage_canary_non_database_error_logs_class_without_payload() {
    let canary = "secret-canary-config-41a70b";
    let log = Log::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(log.clone())
        .with_ansi(false)
        .without_time()
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let error = sqlx::Error::Configuration(Box::new(std::io::Error::other(canary)));
    assert_eq!(storage_error("test.connect", error), Error::Storage);
    assert_eq!(
        storage_error("test.io", sqlx::Error::Io(std::io::Error::other(canary))),
        Error::Storage
    );
    assert_eq!(
        storage_error(
            "test.decode",
            sqlx::Error::Decode(Box::new(std::io::Error::other(canary)))
        ),
        Error::Storage
    );
    assert_eq!(
        storage_error(
            "test.json",
            serde_json::from_str::<serde_json::Value>(canary).unwrap_err()
        ),
        Error::Storage
    );
    let output = log.text();
    assert!(output.contains("operation=\"test.connect\""));
    assert!(output.contains("class=\"connection\""));
    assert!(output.contains("class=\"decode\""));
    assert!(output.contains("class=\"serialization\""));
    assert!(output.contains("sqlstate=\"\""));
    assert!(output.contains("class=\"configuration\""));
    assert!(!output.contains(canary));
}

#[tokio::test(flavor = "current_thread")]
async fn storage_canary_host_filter_suppresses_sqlx_query_and_notice() {
    let canary = "secret-canary-sqlx-notice-81e3a9";
    let url = std::env::var("RUNTRELL_TEST_DATABASE_URL").expect("dedicated database required");
    // Deliberately retain sqlx's default statement logging to exercise the
    // subscriber filter, independently of disable_statement_logging().
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let statement = format!("DO $$ BEGIN RAISE WARNING '{canary}'; END $$");
    let baseline = Log::default();
    let baseline_subscriber = tracing_subscriber::fmt()
        .with_writer(baseline.clone())
        .with_ansi(false)
        .without_time()
        .with_max_level(tracing::Level::TRACE)
        .finish();
    {
        let _guard = tracing::subscriber::set_default(baseline_subscriber);
        sqlx::query(&statement).execute(&pool).await.unwrap();
    }
    let emitted = baseline.text();
    assert!(emitted.contains("sqlx::postgres::notice") && emitted.contains(canary));
    let log = Log::default();
    let targets = Targets::new()
        .with_default(LevelFilter::TRACE)
        .with_target("sqlx", LevelFilter::OFF)
        .with_target("runtrell_postgres::storage", LevelFilter::ERROR);
    let subscriber = tracing_subscriber::registry().with(targets).with(
        tracing_subscriber::fmt::layer()
            .with_writer(log.clone())
            .with_ansi(false)
            .without_time(),
    );
    let _guard = tracing::subscriber::set_default(subscriber);
    sqlx::query(&statement).execute(&pool).await.unwrap();
    assert_eq!(
        storage_error("test.notice", sqlx::Error::PoolClosed),
        Error::Storage
    );
    let output = log.text();
    assert!(output.contains("operation=\"test.notice\""), "{output}");
    assert!(output.contains("class=\"pool\""), "{output}");
    assert!(!output.contains(canary), "sqlx leaked canary: {output}");
    assert!(!output.contains("sqlx::"), "sqlx target leaked: {output}");
}

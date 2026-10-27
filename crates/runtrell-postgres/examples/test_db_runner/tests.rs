// SPDX-License-Identifier: Apache-2.0
use super::*;
use std::{
    path::Path,
    process::{Child, ExitStatus},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

static DB_TESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[test]
fn accepts_only_exact_ownership_marker() {
    let database = "rt_suite_1790000000_0123456789abcdef0123456789abcdef";
    let owner = Owner {
        database: database.into(),
        host: "test-host".into(),
        endpoint_host: "127.0.0.1".into(),
        endpoint_port: 5432,
        child_pid: 42,
    };
    assert!(Owner::parse(database, &owner.comment()).is_some());
    assert!(
        Owner::parse(
            database,
            &owner.comment().replace("test-host", "other:host")
        )
        .is_none()
    );
    assert!(
        Owner::parse(
            "harness_1790000000_0123456789abcdef0123456789abcdef",
            &owner.comment()
        )
        .is_none()
    );
    assert!(Owner::parse(database, "unmarked").is_none());
}

// The outer Cargo target runner gives this test executable a database. Use the
// same endpoint and credentials, but only its maintenance database, for nested
// direct-binary runner invocations and our uniquely named fixture databases.
async fn maintenance() -> Result<(PgConnectOptions, PgConnection, String), Box<dyn Error>> {
    let url = env::var("RUNTRELL_TEST_DATABASE_URL")?;
    let options = PgConnectOptions::from_str(&url)?;
    if !matches!(options.get_host(), "127.0.0.1" | "localhost" | "::1")
        || options.get_socket().is_some()
    {
        return Err("runner tests require loopback PostgreSQL".into());
    }
    let options = options.database("postgres").disable_statement_logging();
    let mut conn = options.connect().await?;
    let (database, user): (String, String) =
        sqlx::query_as("SELECT current_database(), current_user")
            .fetch_one(&mut conn)
            .await?;
    if database != "postgres" {
        return Err("runner tests require the postgres maintenance database".into());
    }
    Ok((options, conn, user))
}

fn fixture_name() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    format!("{PREFIX}{seconds}_{}", uuid::Uuid::new_v4().simple())
}

fn runner_binary() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let target = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                root.join(path)
            }
        })
        .unwrap_or_else(|| root.join("target"));
    target.join("debug/examples/test_db_runner")
}

fn runner_command(options: &PgConnectOptions) -> Command {
    let binary = runner_binary();
    assert!(
        binary.is_file(),
        "build test_db_runner example before its tests: {}",
        binary.display()
    );
    let mut command = Command::new(binary);
    command.env(
        "RUNTRELL_TEST_DATABASE_URL",
        options.to_url_lossy().as_str(),
    );
    command
}

fn wait_for_name(path: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(value) = fs::read_to_string(path) {
            let name = value.trim().to_string();
            if Owner::creation_time(&name).is_some() {
                return name;
            }
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for runner child"
        );
        thread::sleep(Duration::from_millis(25));
    }
}

async fn exists(conn: &mut PgConnection, name: &str) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)")
        .bind(name)
        .fetch_one(conn)
        .await
}

async fn comment(conn: &mut PgConnection, name: &str) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT shobj_description(oid, 'pg_database') FROM pg_database WHERE datname = $1",
    )
    .bind(name)
    .fetch_one(conn)
    .await
}

struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let root =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-db-runner-probes");
        fs::create_dir_all(&root).unwrap();
        let path = root.join(uuid::Uuid::new_v4().simple().to_string());
        fs::create_dir(&path).unwrap();
        Self { path }
    }

    fn at(&self, file: &str) -> PathBuf {
        self.path.join(file)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn entrypoint_resolves_relative_target_from_crate_directory() {
    use std::os::unix::fs::PermissionsExt;

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let scratch = Scratch::new();
    let target = scratch.path.canonicalize().unwrap();
    let examples = target.join("debug/examples");
    fs::create_dir_all(&examples).unwrap();
    let probe = examples.join("test_db_runner");
    // Only the entrypoint's path resolution is under test; this probe uses no DB.
    fs::write(&probe, b"#!/bin/sh\nprintf '%s' \"$1\"\n").unwrap();
    fs::set_permissions(&probe, fs::Permissions::from_mode(0o700)).unwrap();
    let output = Command::new("sh")
        .arg(root.join("tools/test-db-runner.sh"))
        .arg("relative-target-resolved")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("CARGO_TARGET_DIR", target.strip_prefix(&root).unwrap())
        .env("RUNTRELL_TEST_DATABASE_URL", "path-probe-only")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"relative-target-resolved");
}

struct WaitingRunner {
    process: Child,
    release: PathBuf,
    stderr: PathBuf,
    finished: bool,
}

impl WaitingRunner {
    fn start(options: &PgConnectOptions, scratch: &Scratch) -> Self {
        let release = scratch.at("release");
        let name = scratch.at("name");
        let stderr = scratch.at("stderr");
        let process = runner_command(options)
            .args([
                "sh", "-c",
                "url=${RUNTRELL_TEST_DATABASE_URL%%\\?*}; printf '%s\\n' \"${url##*/}\" > \"$1\"; count=0; while [ ! -e \"$2\" ] && [ \"$count\" -lt 200 ]; do sleep 0.1; count=$((count+1)); done",
                "sh",
            ])
            .arg(name)
            .arg(&release)
            .stdin(Stdio::null())
            .stderr(Stdio::from(fs::File::create(&stderr).unwrap()))
            .spawn()
            .unwrap();
        Self {
            process,
            release,
            stderr,
            finished: false,
        }
    }

    fn finish(&mut self) -> (ExitStatus, String) {
        fs::write(&self.release, b"go").unwrap();
        let status = self.process.wait().unwrap();
        self.finished = true;
        (status, fs::read_to_string(&self.stderr).unwrap())
    }
}

impl Drop for WaitingRunner {
    fn drop(&mut self) {
        let _ = fs::write(&self.release, b"go");
        if !self.finished {
            let _ = self.process.kill();
            let _ = self.process.wait();
        }
    }
}

#[tokio::test]
async fn live_child_survives_other_runner_then_sigkill_is_reclaimed() -> Result<(), Box<dyn Error>>
{
    let _serial = DB_TESTS.lock().await;
    let (options, mut conn, user) = maintenance().await?;
    let scratch = Scratch::new();
    let mut waiting = WaitingRunner::start(&options, &scratch);
    let name = wait_for_name(&scratch.at("name"));
    let owner = Owner::parse(
        &name,
        &comment(&mut conn, &name).await?.ok_or("missing comment")?,
    )
    .ok_or("missing owner marker")?;
    assert!(child_alive(owner.child_pid));
    assert!(!reclaim(&mut conn, &owner, &user, &owner.host, &options, false).await?);
    assert!(runner_command(&options).arg("true").status()?.success());
    assert!(
        exists(&mut conn, &name).await?,
        "another runner reaped a live child"
    );

    waiting.process.kill()?; // SIGKILL of the parent runner, not its child.
    waiting.process.wait()?;
    waiting.finished = true;
    assert!(child_alive(owner.child_pid));
    assert!(runner_command(&options).arg("true").status()?.success());
    assert!(
        exists(&mut conn, &name).await?,
        "orphan's live child was reaped"
    );
    fs::write(&waiting.release, b"go")?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while child_alive(owner.child_pid) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(!child_alive(owner.child_pid), "orphan child did not exit");
    assert!(runner_command(&options).arg("true").status()?.success());
    assert!(
        !exists(&mut conn, &name).await?,
        "next runner did not reclaim SIGKILL orphan"
    );
    assert!(!intent_path(&name).exists());
    Ok(())
}

#[tokio::test]
async fn creation_gap_requires_lock_and_exact_intent() -> Result<(), Box<dyn Error>> {
    let _serial = DB_TESTS.lock().await;
    let (options, mut conn, user) = maintenance().await?;
    let mut other = options.connect().await?;
    let name = fixture_name();
    let host = local_host()?;
    let intent = intent_text(&name, &host, &options, &user);
    sqlx::query("SELECT pg_advisory_lock(hashtextextended($1, 0))")
        .bind(&name)
        .execute(&mut conn)
        .await?;
    write_intent(&name, &intent)?;
    sqlx::query(&format!("CREATE DATABASE {name}"))
        .execute(&mut conn)
        .await?;
    let checked = async {
        let locked = !reclaim_unmarked(&mut other, &name, &user, &intent).await?
            && exists(&mut other, &name).await?;
        sqlx::query("SELECT pg_advisory_unlock(hashtextextended($1, 0))")
            .bind(&name)
            .execute(&mut conn)
            .await?;
        let wrong_intent = !reclaim_unmarked(&mut other, &name, &user, "wrong intent").await?
            && exists(&mut other, &name).await?;
        let recovered = reclaim_unmarked(&mut other, &name, &user, &intent).await?;
        Ok::<_, Box<dyn Error>>((locked, wrong_intent, recovered))
    }
    .await;
    // Only this exact fixture's lock, database and intent are ever removed.
    let _ = sqlx::query("SELECT pg_advisory_unlock(hashtextextended($1, 0))")
        .bind(&name)
        .execute(&mut conn)
        .await;
    if exists(&mut conn, &name).await? {
        sqlx::query(&format!("DROP DATABASE {name}"))
            .execute(&mut conn)
            .await?;
    }
    fs::remove_file(intent_path(&name))?;
    let (locked, wrong_intent, recovered) = checked?;
    assert!(locked, "creation lock did not protect unmarked database");
    assert!(
        wrong_intent,
        "mismatched intent did not protect unmarked database"
    );
    assert!(
        recovered,
        "matching intent did not recover unmarked database"
    );
    Ok(())
}

#[tokio::test]
async fn ordinary_failure_cleans_up_but_active_connection_reports_cleanup_failure()
-> Result<(), Box<dyn Error>> {
    let _serial = DB_TESTS.lock().await;
    let (options, mut conn, _) = maintenance().await?;
    let scratch = Scratch::new();
    let failed_name = scratch.at("failed-name");
    let status = runner_command(&options)
        .args([
            "sh",
            "-c",
            "url=${RUNTRELL_TEST_DATABASE_URL%%\\?*}; printf '%s\\n' \"${url##*/}\" > \"$1\"; exit 37",
            "sh",
        ])
        .arg(&failed_name)
        .status()?;
    assert_eq!(status.code(), Some(37));
    assert!(!exists(&mut conn, &wait_for_name(&failed_name)).await?);

    let mut waiting = WaitingRunner::start(&options, &scratch);
    let name = wait_for_name(&scratch.at("name"));
    let active = options.clone().database(&name).connect().await?;
    let (status, stderr) = waiting.finish();
    assert!(
        !status.success(),
        "cleanup succeeded with an active connection"
    );
    assert!(stderr.contains("test database cleanup was incomplete"));
    assert!(exists(&mut conn, &name).await?);
    sqlx::Connection::close(active).await?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let connections: i64 =
            sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname = $1")
                .bind(&name)
                .fetch_one(&mut conn)
                .await?;
        if connections == 0 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "fixture connection did not close"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(runner_command(&options).arg("true").status()?.success());
    assert!(!exists(&mut conn, &name).await?);
    Ok(())
}

#[tokio::test]
async fn foreign_marker_and_other_owner_are_preserved() -> Result<(), Box<dyn Error>> {
    let _serial = DB_TESTS.lock().await;
    let (options, mut conn, user) = maintenance().await?;
    let host = local_host()?;
    let name = fixture_name();
    let other = fixture_name();
    let role = format!("rt_runner_owner_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE ROLE {role}"))
        .execute(&mut conn)
        .await?;
    sqlx::query(&format!("CREATE DATABASE {name}"))
        .execute(&mut conn)
        .await?;
    sqlx::query(&format!("CREATE DATABASE {other} OWNER {role}"))
        .execute(&mut conn)
        .await?;
    let mut exited = Command::new("true").spawn()?;
    let exited_pid = exited.id();
    assert!(exited.wait()?.success());
    let owner = Owner {
        database: other.clone(),
        host: host.clone(),
        endpoint_host: options.get_host().into(),
        endpoint_port: options.get_port(),
        child_pid: exited_pid,
    };
    sqlx::query(&format!(
        "COMMENT ON DATABASE {name} IS 'another-runner:v1'"
    ))
    .execute(&mut conn)
    .await?;
    sqlx::query(&format!(
        "COMMENT ON DATABASE {other} IS '{}'",
        owner.comment()
    ))
    .execute(&mut conn)
    .await?;
    let checked = async {
        let foreign = marked_database(&mut conn, &name, &user).await?.is_none();
        let other_owner = !reclaim(&mut conn, &owner, &user, &host, &options, true).await?;
        let runner_ok = runner_command(&options).arg("true").status()?.success();
        let retained = exists(&mut conn, &name).await? && exists(&mut conn, &other).await?;
        Ok::<_, Box<dyn Error>>((foreign, other_owner, runner_ok, retained))
    }
    .await;
    sqlx::query(&format!("DROP DATABASE {name}"))
        .execute(&mut conn)
        .await?;
    sqlx::query(&format!("DROP DATABASE {other}"))
        .execute(&mut conn)
        .await?;
    sqlx::query(&format!("DROP ROLE {role}"))
        .execute(&mut conn)
        .await?;
    let (foreign, other_owner, runner_ok, retained) = checked?;
    assert!(foreign, "foreign marker was accepted");
    assert!(other_owner, "other owner was accepted");
    assert!(runner_ok && retained, "runner removed a foreign fixture");
    Ok(())
}

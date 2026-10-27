// SPDX-License-Identifier: Apache-2.0
//! Cargo target runner for disposable, per-executable PostgreSQL databases.
//! Build this example first, then set CARGO_TARGET_<TRIPLE>_RUNNER to its path.

use sqlx::{ConnectOptions, PgConnection, postgres::PgConnectOptions};
use std::{
    env,
    error::Error,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::PathBuf,
    process::{Command, Stdio},
    str::FromStr,
};

const PREFIX: &str = "rt_suite_";
const MARKER: &str = "runtrell-test-db:v1";

#[derive(Debug)]
struct Owner {
    database: String,
    host: String,
    endpoint_host: String,
    endpoint_port: u16,
    child_pid: u32,
}

impl Owner {
    fn comment(&self) -> String {
        format!(
            "{MARKER}:{}:{}:{}:{}:{}",
            self.database, self.host, self.endpoint_host, self.endpoint_port, self.child_pid
        )
    }

    fn parse(database: &str, comment: &str) -> Option<Self> {
        Self::creation_time(database)?;
        let rest = comment.strip_prefix(&format!("{MARKER}:{database}:"))?;
        let (rest, pid) = rest.rsplit_once(':')?;
        let (rest, port) = rest.rsplit_once(':')?;
        let (host, endpoint_host) = rest.split_once(':')?;
        if host.is_empty()
            || host.contains(':')
            || host.contains('\n')
            || !matches!(endpoint_host, "127.0.0.1" | "localhost" | "::1")
        {
            return None;
        }
        let endpoint_port = port.parse::<u16>().ok().filter(|port| *port > 0)?;
        let child_pid = pid.parse::<u32>().ok().filter(|pid| *pid > 0)?;
        Some(Self {
            database: database.into(),
            host: host.into(),
            endpoint_host: endpoint_host.into(),
            endpoint_port,
            child_pid,
        })
    }

    fn creation_time(database: &str) -> Option<u64> {
        let (seconds, uuid) = database.strip_prefix(PREFIX)?.split_once('_')?;
        if seconds.len() != 10
            || !seconds.bytes().all(|c| c.is_ascii_digit())
            || uuid.len() != 32
            || !uuid.bytes().all(|c| c.is_ascii_hexdigit())
        {
            return None;
        }
        seconds.parse().ok()
    }
}

fn local_host() -> Result<String, Box<dyn Error>> {
    let output = Command::new("hostname").output()?;
    if !output.status.success() {
        return Err("hostname failed".into());
    }
    let host = String::from_utf8(output.stdout)?.trim().to_owned();
    if host.is_empty() || host.contains(':') || host.contains('\n') {
        return Err("invalid local hostname".into());
    }
    Ok(host)
}

fn child_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return true;
    };
    // SAFETY: kill with signal 0 only checks process existence and permissions.
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

fn intent_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-db-intents")
}

fn intent_path(name: &str) -> PathBuf {
    intent_dir().join(name)
}

fn intent_text(name: &str, host: &str, options: &PgConnectOptions, user: &str) -> String {
    format!(
        "{MARKER}:intent:{name}\n{host}\n{}\n{}\n{}\n{user}\n",
        options.get_host(),
        options.get_port(),
        options.get_database().unwrap_or_default()
    )
}

fn write_intent(name: &str, text: &str) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(intent_dir())?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(intent_path(name))?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    fs::File::open(intent_dir())?.sync_all()?;
    Ok(())
}

fn matching_intent(name: &str, text: &str) -> bool {
    let Ok(mut file) = fs::File::open(intent_path(name)) else {
        return false;
    };
    let mut found = String::new();
    file.read_to_string(&mut found).is_ok() && found == text
}

async fn marked_database(
    conn: &mut PgConnection,
    name: &str,
    user: &str,
) -> Result<Option<Owner>, sqlx::Error> {
    let row: Option<(String, Option<String>)> = sqlx::query_as(
        "SELECT pg_get_userbyid(datdba), shobj_description(oid, 'pg_database') \
         FROM pg_database WHERE datname = $1 AND NOT datistemplate",
    )
    .bind(name)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((owner, comment)) = row else {
        return Ok(None);
    };
    if owner != user {
        return Ok(None);
    }
    Ok(comment.and_then(|comment| Owner::parse(name, &comment)))
}

async fn reclaim(
    conn: &mut PgConnection,
    owner: &Owner,
    user: &str,
    host: &str,
    options: &PgConnectOptions,
    own_child_done: bool,
) -> Result<bool, Box<dyn Error>> {
    if owner.host != host
        || owner.endpoint_host != options.get_host()
        || owner.endpoint_port != options.get_port()
        || (!own_child_done && child_alive(owner.child_pid))
    {
        return Ok(false);
    }
    let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock(hashtextextended($1, 0))")
        .bind(&owner.database)
        .fetch_one(&mut *conn)
        .await?;
    if !locked {
        return Ok(false);
    }
    let result = async {
        let Some(found) = marked_database(conn, &owner.database, user).await? else {
            return Ok(false);
        };
        if found.comment() != owner.comment() {
            return Ok(false);
        }
        let active: i64 =
            sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname = $1")
                .bind(&owner.database)
                .fetch_one(&mut *conn)
                .await?;
        if active != 0 {
            return Ok(false);
        }
        // Database names were strictly parsed above. DROP cannot be parameterized.
        sqlx::query(&format!("DROP DATABASE {}", owner.database))
            .execute(&mut *conn)
            .await?;
        Ok::<bool, sqlx::Error>(true)
    }
    .await;
    sqlx::query("SELECT pg_advisory_unlock(hashtextextended($1, 0))")
        .bind(&owner.database)
        .execute(&mut *conn)
        .await?;
    Ok(result?)
}

async fn reclaim_unmarked(
    conn: &mut PgConnection,
    name: &str,
    user: &str,
    intent: &str,
) -> Result<bool, Box<dyn Error>> {
    // The creator takes the same lock before persisting intent and CREATE.
    // An unmarked database therefore cannot contain a started test child.
    if Owner::creation_time(name).is_none() || !matching_intent(name, intent) {
        return Ok(false);
    }
    let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock(hashtextextended($1, 0))")
        .bind(name)
        .fetch_one(&mut *conn)
        .await?;
    if !locked {
        return Ok(false);
    }
    let result = async {
        let row: Option<(String, Option<String>)> = sqlx::query_as(
            "SELECT pg_get_userbyid(datdba), shobj_description(oid, 'pg_database') \
             FROM pg_database WHERE datname = $1 AND NOT datistemplate",
        )
        .bind(name)
        .fetch_optional(&mut *conn)
        .await?;
        if !matches!(row, Some((ref owner, None)) if owner == user)
            || !matching_intent(name, intent)
        {
            return Ok(false);
        }
        let active: i64 =
            sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname = $1")
                .bind(name)
                .fetch_one(&mut *conn)
                .await?;
        if active != 0 {
            return Ok(false);
        }
        sqlx::query(&format!("DROP DATABASE {name}"))
            .execute(&mut *conn)
            .await?;
        Ok::<bool, sqlx::Error>(true)
    }
    .await;
    sqlx::query("SELECT pg_advisory_unlock(hashtextextended($1, 0))")
        .bind(name)
        .execute(&mut *conn)
        .await?;
    Ok(result?)
}

async fn remove_missing_intents(
    conn: &mut PgConnection,
    host: &str,
    options: &PgConnectOptions,
    user: &str,
) -> Result<(), Box<dyn Error>> {
    let Ok(entries) = fs::read_dir(intent_dir()) else {
        return Ok(());
    };
    for entry in entries {
        let entry = entry?;
        let Some(name) = entry.file_name().into_string().ok() else {
            continue;
        };
        if Owner::creation_time(&name).is_none() {
            continue;
        }
        let text = intent_text(&name, host, options, user);
        if !matching_intent(&name, &text) {
            continue;
        }
        let locked: bool =
            sqlx::query_scalar("SELECT pg_try_advisory_lock(hashtextextended($1, 0))")
                .bind(&name)
                .fetch_one(&mut *conn)
                .await?;
        if !locked {
            continue;
        }
        let found: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)")
                .bind(&name)
                .fetch_one(&mut *conn)
                .await?;
        if !found && matching_intent(&name, &text) {
            fs::remove_file(intent_path(&name))?;
        }
        sqlx::query("SELECT pg_advisory_unlock(hashtextextended($1, 0))")
            .bind(&name)
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

async fn run() -> Result<i32, Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let executable = args.next().ok_or("expected executable to run")?;
    let Ok(url) = env::var("RUNTRELL_TEST_DATABASE_URL") else {
        // The isolation jobs have no PostgreSQL service or database tests.
        let status = Command::new(executable).args(args).status()?;
        return Ok(status.code().unwrap_or(1));
    };
    let options = PgConnectOptions::from_str(&url)?.disable_statement_logging();
    if options.get_database() != Some("postgres")
        || !matches!(options.get_host(), "127.0.0.1" | "localhost" | "::1")
        || options.get_socket().is_some()
    {
        return Err("runner requires a loopback PostgreSQL URL ending in /postgres".into());
    }
    let mut conn = options.connect().await?;
    let actual: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&mut conn)
        .await?;
    if actual != "postgres" {
        return Err("connected to unexpected maintenance database".into());
    }
    let user: String = sqlx::query_scalar("SELECT current_user")
        .fetch_one(&mut conn)
        .await?;
    let host = local_host()?;
    let old: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT datname, shobj_description(oid, 'pg_database') FROM pg_database \
         WHERE datname LIKE 'rt_suite_%' AND NOT datistemplate",
    )
    .fetch_all(&mut conn)
    .await?;
    for (name, comment) in old {
        let reclaimed = match comment {
            Some(comment) => match Owner::parse(&name, &comment) {
                Some(owner) => reclaim(&mut conn, &owner, &user, &host, &options, false).await?,
                None => false,
            },
            None => {
                reclaim_unmarked(
                    &mut conn,
                    &name,
                    &user,
                    &intent_text(&name, &host, &options, &user),
                )
                .await?
            }
        };
        if reclaimed {
            let _ = fs::remove_file(intent_path(&name));
            eprintln!("reclaimed previous test database {name}");
        }
    }
    remove_missing_intents(&mut conn, &host, &options, &user).await?;

    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let name = format!("{PREFIX}{created}_{}", uuid::Uuid::new_v4().simple());
    // Lock before CREATE; another runner cannot reap the unmarked creation gap.
    sqlx::query("SELECT pg_advisory_lock(hashtextextended($1, 0))")
        .bind(&name)
        .execute(&mut conn)
        .await?;
    write_intent(&name, &intent_text(&name, &host, &options, &user))?;
    sqlx::query(&format!("CREATE DATABASE {name}"))
        .execute(&mut conn)
        .await?;

    // The shell waits on stdin before exec, so its PID can be recorded before
    // any test code touches the database. On parent SIGKILL, EOF makes it exit.
    let mut child = Command::new("sh")
        .arg("-c")
        .arg("IFS= read -r gate || exit 125; exec \"$@\"")
        .arg("sh")
        .arg(executable)
        .args(args)
        .env(
            "RUNTRELL_TEST_DATABASE_URL",
            options.clone().database(&name).to_url_lossy().as_str(),
        )
        .stdin(Stdio::piped())
        .spawn()?;
    let owner = Owner {
        database: name.clone(),
        host,
        endpoint_host: options.get_host().into(),
        endpoint_port: options.get_port(),
        child_pid: child.id(),
    };
    let comment = owner.comment().replace('\'', "''");
    if let Err(error) = sqlx::query(&format!("COMMENT ON DATABASE {name} IS '{comment}'"))
        .execute(&mut conn)
        .await
    {
        drop(child.stdin.take());
        let _ = child.wait();
        return Err(error.into());
    }
    child
        .stdin
        .take()
        .ok_or("child input unavailable")?
        .write_all(b"go\n")?;
    let status = child.wait()?;
    let mut removed = false;
    for _ in 0..50 {
        if reclaim(&mut conn, &owner, &user, &owner.host, &options, true).await? {
            removed = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    if !removed {
        eprintln!("test database {name} remains; next runner will retry after child exits");
        return Err("test database cleanup was incomplete".into());
    }
    // The creation lock stays held through DROP. Reclaim took and released a
    // reentrant lock; release the original lock only after the database is gone.
    sqlx::query("SELECT pg_advisory_unlock(hashtextextended($1, 0))")
        .bind(&name)
        .execute(&mut conn)
        .await?;
    fs::remove_file(intent_path(&name))?;
    Ok(status.code().unwrap_or(1))
}

fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    match runtime.block_on(run()) {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("test database runner: {error}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
#[path = "test_db_runner/tests.rs"]
mod tests;

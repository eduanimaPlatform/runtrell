// SPDX-License-Identifier: Apache-2.0
use runtrell::Error;
use std::error::Error as StdError;

/// Record a storage failure without exposing an error's message or its inputs.
/// `operation` must be a static adapter operation name, never a caller value.
/// The generated correlation ID identifies this diagnostic event. Hosts that
/// need request-wide correlation supply a safe field on the active tracing span.
pub fn storage_error<E: StdError + 'static>(operation: &'static str, error: E) -> Error {
    let (class, state) = if let Some(sqlx) = (&error as &dyn StdError).downcast_ref::<sqlx::Error>()
    {
        classify_sqlx(sqlx)
    } else if (&error as &dyn StdError).is::<serde_json::Error>() {
        ("serialization", None)
    } else {
        ("conversion", None)
    };
    let sqlstate = state.unwrap_or_default();
    let correlation_id = uuid::Uuid::new_v4();
    tracing::error!(target: "runtrell_postgres::storage", operation, sqlstate, class, %correlation_id, "storage failure");
    Error::Storage
}

fn classify_sqlx(error: &sqlx::Error) -> (&'static str, Option<String>) {
    match error {
        sqlx::Error::Database(db) => {
            let state = db.code();
            let state = state.as_deref().filter(|s| {
                s.len() == 5
                    && s.bytes()
                        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
            });
            let class = match state.map(|s| &s[..2]) {
                Some("23") => "constraint",
                Some("22") => "data_exception",
                Some("40") => "transaction",
                Some("08") => "connection",
                _ => "database",
            };
            (class, state.map(str::to_owned))
        }
        sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed => ("pool", None),
        sqlx::Error::Configuration(_) => ("configuration", None),
        sqlx::Error::Io(_) | sqlx::Error::Tls(_) => ("connection", None),
        sqlx::Error::Decode(_) | sqlx::Error::ColumnDecode { .. } => ("decode", None),
        _ => ("database", None),
    }
}

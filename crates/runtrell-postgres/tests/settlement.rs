// SPDX-License-Identifier: Apache-2.0
//! Owner settlement recovery on real PostgreSQL: a restarted store on an independent pool
//! sees the same operator-action wait and the single recorded settlement.
#[allow(dead_code)]
mod support;
use runtrell::*;
use runtrell_postgres::PgStore;
use runtrell_testkit::settlement_contracts;
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;

async fn two_stores() -> (Arc<PgStore>, Arc<PgStore>) {
    let url = std::env::var("RUNTRELL_TEST_DATABASE_URL").expect("test DB required");
    let schema = format!("settlement_{}", uuid::Uuid::new_v4().simple());
    let first = PgPoolOptions::new()
        .max_connections(8)
        .connect(&url)
        .await
        .unwrap();
    let store = Arc::new(
        PgStore::initialize(first, &schema, Capacity::default())
            .await
            .unwrap(),
    );
    let second = PgPoolOptions::new()
        .max_connections(8)
        .connect(&url)
        .await
        .unwrap();
    let restarted = Arc::new(
        PgStore::initialize(second, &schema, Capacity::default())
            .await
            .unwrap(),
    );
    (store, restarted)
}

#[test]
fn image_usage_operator_recovery() {
    support::block_on(async {
        let (store, restarted) = two_stores().await;
        settlement_contracts::image_usage_operator_recovery(store, restarted).await;
    });
}
#[test]
fn wait_follows_the_lookup_window() {
    support::block_on(async {
        let (store, _) = two_stores().await;
        settlement_contracts::wait_follows_the_lookup_window(store).await;
    });
}

#[test]
fn initialize_adds_the_audit_column_to_an_existing_runtime_and_connect_requires_it() {
    support::block_on(async {
        let url = std::env::var("RUNTRELL_TEST_DATABASE_URL").expect("test DB required");
        let (store, _) = two_stores().await;
        let _ = store;
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .unwrap();
        let schema = format!("settlement_{}", uuid::Uuid::new_v4().simple());
        PgStore::initialize(pool.clone(), &schema, Capacity::default())
            .await
            .unwrap();
        // A runtime created before the column existed.
        sqlx::query(&format!(
            "ALTER TABLE \"{schema}\".attempts DROP COLUMN recovery"
        ))
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(
            PgStore::connect(pool.clone(), &schema, Capacity::default())
                .await
                .err(),
            Some(Error::NotFound)
        );
        PgStore::initialize(pool.clone(), &schema, Capacity::default())
            .await
            .unwrap();
        PgStore::connect(pool.clone(), &schema, Capacity::default())
            .await
            .unwrap();
        // Re-running initialization is a no-op for the existing column.
        PgStore::initialize(pool, &schema, Capacity::default())
            .await
            .unwrap();
    });
}

#[test]
fn lost_reply_keeps_lookup_recovery() {
    support::block_on(async {
        let (store, restarted) = two_stores().await;
        settlement_contracts::lost_reply_keeps_lookup_recovery(store, restarted).await;
    });
}

#[test]
fn settle_never_resumes() {
    support::block_on(async {
        let (store, restarted) = two_stores().await;
        settlement_contracts::settle_never_resumes(store, restarted).await;
    });
}

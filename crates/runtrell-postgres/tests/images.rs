// SPDX-License-Identifier: Apache-2.0
use runtrell::{data::*, *};
use runtrell_postgres::PgStore;
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;

#[tokio::test]
async fn images_reopen_artifacts_and_independent_pg_pool_with_one_settlement_row_per_send() {
    let url = std::env::var("RUNTRELL_TEST_DATABASE_URL").expect("image Pg tests never skip");
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .unwrap();
    let schema = format!("images_{}", uuid::Uuid::new_v4().simple());
    let first = Arc::new(
        PgStore::open(pool.clone(), &schema, Capacity::default())
            .await
            .unwrap(),
    );
    let independent = PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .unwrap();
    let reopened = Arc::new(
        PgStore::open(independent, &schema, Capacity::default())
            .await
            .unwrap(),
    );
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target")
        .join(&schema);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let data = DataStore::local(&root, StoragePolicy::default()).unwrap();
    let reloaded = DataStore::local(&root, StoragePolicy::default()).unwrap();
    runtrell_testkit::image_contracts::workflow_reopened(first, data, reopened, reloaded).await;
    let (entries,settled):(i64,i64) = sqlx::query_as(&format!("SELECT count(*),count(*) FILTER (WHERE settled AND NOT unknown) FROM {schema}.budget_entries")).fetch_one(&pool).await.unwrap();
    assert_eq!((entries, settled), (5, 5));
    let sends: i64 = sqlx::query_scalar(&format!(
        "SELECT sum(send_count)::bigint FROM {schema}.attempts"
    ))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(sends, 5);
    std::fs::remove_dir_all(root).unwrap();
}

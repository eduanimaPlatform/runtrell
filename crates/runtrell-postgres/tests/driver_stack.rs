// SPDX-License-Identifier: Apache-2.0
use runtrell::{Capacity, data::*};
use runtrell_postgres::PgStore;
use runtrell_testkit::{driver_contracts, image_contracts};
use sqlx::postgres::PgPoolOptions;
use std::{future::Future, sync::Arc};

fn small_stack<F: Future<Output = ()>>(
    run: impl FnOnce(Arc<PgStore>, Arc<PgStore>) -> F + Send + 'static,
) {
    // Half Tokio's default 2 MiB worker stack; all driver polling and Pg
    // checkpoint decoding stay on this explicitly bounded executor thread.
    std::thread::Builder::new()
        .name("pg-driver-small-stack".into())
        .stack_size(1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let (store, reopened) = runtime.block_on(Box::pin(async {
                let url = std::env::var("RUNTRELL_TEST_DATABASE_URL")
                    .expect("small-stack Pg tests never skip");
                let schema = format!("driver_stack_{}", uuid::Uuid::new_v4().simple());
                let pool = PgPoolOptions::new().connect(&url).await.unwrap();
                let store = Arc::new(
                    PgStore::open(pool, &schema, Capacity::default())
                        .await
                        .unwrap(),
                );
                let independent = PgPoolOptions::new().connect(&url).await.unwrap();
                let reopened = Arc::new(
                    PgStore::open(independent, &schema, Capacity::default())
                        .await
                        .unwrap(),
                );
                (store, reopened)
            }));
            runtime.block_on(Box::pin(run(store, reopened)));
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn model_tool_compaction_and_recovery_on_small_stack() {
    small_stack(driver_contracts::automatic_compaction);
}

#[test]
fn image_attachments_and_saved_result_recovery_on_small_stack() {
    small_stack(|store, reopened| {
        let data = DataStore::memory(StoragePolicy::default());
        image_contracts::workflow_reopened(store, data.clone(), reopened, data)
    });
}

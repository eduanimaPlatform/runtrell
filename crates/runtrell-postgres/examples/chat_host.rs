// SPDX-License-Identifier: Apache-2.0
//! Durable chat messages with one finite Run per reply; fake model, real PostgreSQL.
#[path = "chat_host/host.rs"]
mod host;

fn main() {
    if let Ok(schema) = std::env::var("RUNTRELL_CHAT_SCHEMA") {
        let phase = std::env::var("RUNTRELL_CHAT_PHASE").unwrap();
        host::block_on(host::phase(&schema, &phase));
    } else {
        host::processes(&[]);
        println!("chat host: process restart, history, idle and recovery contracts passed");
    }
}

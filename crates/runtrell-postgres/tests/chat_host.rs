// SPDX-License-Identifier: Apache-2.0
#[path = "../examples/chat_host/host.rs"]
mod host;

#[test]
fn chat_host_contract_across_processes() {
    if let Ok(schema) = std::env::var("RUNTRELL_CHAT_SCHEMA") {
        let phase = std::env::var("RUNTRELL_CHAT_PHASE").unwrap();
        host::block_on(host::phase(&schema, &phase));
    } else {
        host::processes(&[
            "--exact",
            "chat_host_contract_across_processes",
            "--nocapture",
        ]);
    }
}

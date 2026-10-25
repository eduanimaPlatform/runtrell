// SPDX-License-Identifier: Apache-2.0
//! Fake model plus two business tools; no workspace or execution backend.
use runtrell::{memory::MemoryStore, *};
use runtrell_testkit::*;
use serde_json::json;
use std::sync::Arc;
fn main() -> Result<()> {
    block_on(async {
        let store = Arc::new(MemoryStore::default());
        let budget = store
            .create_account("demo", Amount::new(1000, 1000), None)
            .await?;
        let handle = store
            .start(RunSpec::new(
                "demo",
                "inquiry-1",
                budget,
                "Answer the inquiry using search and case facts.",
            ))
            .await?;
        let model = FakeModel::scripted(vec![
            FakeModel::calls(vec![ToolCall {
                id: "search-1".into(),
                name: "search".into(),
                arguments: json!({"query":"delivery"}),
            }]),
            FakeModel::calls(vec![ToolCall {
                id: "case-1".into(),
                name: "case_read".into(),
                arguments: json!({"case":"demo"}),
            }]),
            FakeModel::decision(json!({"decision":"answered"})),
        ]);
        let runtime = runtime(
            store.clone(),
            model,
            vec![BusinessTool::new("search"), BusinessTool::new("case_read")],
        );
        let owner = store.claim(&handle, "demo-worker", 0, 100).await?;
        let result = runtime.drive(&owner).await?;
        assert_eq!(result.state, RunState::Terminal(Terminal::Succeeded));
        println!("{:?}: {}", result.state, result.output.unwrap());
        Ok(())
    })
}

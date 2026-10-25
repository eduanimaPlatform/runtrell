// SPDX-License-Identifier: Apache-2.0
//! A second product replaces both ModelPort and Tool using only the public SDK.
use runtrell::{memory::MemoryStore, *};
use runtrell_testkit::{DecisionFinalizer, DecisionValidator, FakePolicy, TestClock, block_on};
use serde_json::{Value, json};
use std::sync::Arc;
struct WarehouseModel;
impl ModelPort for WarehouseModel {
    fn limits(&self) -> ModelLimits {
        ModelLimits {
            model: "warehouse".into(),
            context: 256,
            max_output: 8,
            min_max_output_tokens: 1,
            timeout: 10,
            price: Some(Price {
                model: "warehouse".into(),
                input: 0,
                cached: 0,
                output: 0,
                reasoning: 0,
                image_input: 0,
                cached_image_input: 0,
                image_output: 0,
            }),
        }
    }
    fn input_upper_bound(&self, _: &ModelRequest) -> Result<u64> {
        Ok(8)
    }
    fn recovery(&self) -> Option<(String, u64)> {
        None
    }
    async fn invoke(&self, _: &ToolContext, request: ModelRequest) -> Result<ExternalResult> {
        let calls = if request.messages.iter().any(|m| m.role == "tool") {
            vec![]
        } else {
            vec![ToolCall {
                id: "stock-1".into(),
                name: "stock".into(),
                arguments: json!({"sku":"demo"}),
            }]
        };
        let reply = ModelReply {
            text: if calls.is_empty() {
                json!({"decision":"in_stock"}).to_string()
            } else {
                String::new()
            },
            calls,
            complete: true,
            metadata: json!({"host":"warehouse"}),
        };
        let mut usage = Usage::measured(2, 1, 0);
        usage.model = "warehouse".into();
        Ok(ExternalResult::Applied {
            output: serde_json::to_value(reply).unwrap(),
            usage,
            external_id: None,
        })
    }
    async fn lookup(&self, _: &ToolContext, _: ModelRequest) -> Result<Lookup> {
        Ok(Lookup::Unknown)
    }
}
struct StockTool;
impl Tool for StockTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: "stock".into(),
            version: "2026-10".into(),
            account: "warehouse-read".into(),
            permission: "stock.read".into(),
            capability: "trusted-native".into(),
            input_schema: json!({"type":"object","properties":{"sku":{"type":"string"}},"required":["sku"],"additionalProperties":false}),
            output_schema: json!({"type":"object","properties":{"quantity":{"type":"integer"}},"required":["quantity"]}),
            side_effect: false,
            timeout: 5,
            max_output_bytes: 128,
            reserve: Amount::default(),
            quantities: Default::default(),
            conflict_scope: None,
            retry: RetryPolicy {
                max_attempts: 1,
                backoff: 1,
                max_elapsed: 5,
            },
        }
    }
    async fn invoke(&self, _: &ToolContext, _: Value) -> Result<ExternalResult> {
        Ok(ExternalResult::Applied {
            output: json!({"quantity":7}),
            usage: Usage::measured(0, 0, 0),
            external_id: None,
        })
    }
    async fn lookup(&self, _: &ToolContext, _: Value) -> Result<Lookup> {
        Ok(Lookup::Unknown)
    }
}
fn main() -> Result<()> {
    block_on(async {
        let store = Arc::new(MemoryStore::default());
        let budget = store
            .create_account("warehouse", Amount::new(100, 100), None)
            .await?;
        let h = store
            .start(RunSpec::new(
                "warehouse",
                "inventory-1",
                budget,
                "Check the stock.",
            ))
            .await?;
        let c = store.claim(&h, "warehouse-worker", 0, 100).await?;
        let runtime = Runtime {
            store,
            model: WarehouseModel,
            tools: vec![StockTool],
            policy: FakePolicy::default(),
            clock: TestClock::default(),
            validator: DecisionValidator::default(),
            finalizer: DecisionFinalizer,
        };
        let result = runtime.drive(&c).await?;
        assert_eq!(result.state, RunState::Terminal(Terminal::Succeeded));
        println!("{}", result.output.unwrap());
        Ok(())
    })
}

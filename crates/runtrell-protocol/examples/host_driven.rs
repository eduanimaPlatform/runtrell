// SPDX-License-Identifier: Apache-2.0
//! An in-process fake host: one lookup, then a structured judgment.
//! No provider, credentials, files, network or sandbox is needed.
use runtrell_protocol::{
    completion::{CompletionRequest, CompletionResponse, FinishReason, ToolDefinition, Usage},
    message::{AssistantContent, Origin, ToolName},
    run::{
        AgentRun, AgentRunStep, ModelTurn, PendingToolCall, prepare::prepare_request, spec::RunSpec,
    },
    tool::{ToolOutput, ToolResult},
};
use serde_json::json;
use std::sync::Arc;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let spec = RunSpec::new();
    let mut run = AgentRun::from_spec(
        &spec,
        "Check a support case and return its disposition.",
        None,
    );
    let lookup = Arc::new(ToolDefinition::new(
        ToolName::new("lookup_case")?,
        "Read a case",
        json!({"type":"object"}),
    ));
    loop {
        match run.next_step()? {
            AgentRunStep::CallModel {
                prompt,
                history,
                turn,
            } => {
                let prepared = prepare_request(
                    &spec,
                    &Default::default(),
                    &history,
                    vec![lookup.clone()],
                    None,
                    None,
                )?;
                // The host receives a complete model request and may use any adapter.
                let request = prepared
                    .clone()
                    .apply(CompletionRequest::new((*prompt).clone()));
                request.validate_message_content()?;
                let choice = if turn == 1 {
                    vec![AssistantContent::tool_call(
                        "c1",
                        lookup.name.clone(),
                        json!({}),
                    )]
                } else {
                    vec![AssistantContent::text(
                        r#"{"disposition":"answered","evidence":"case-1"}"#,
                    )]
                };
                let reply = CompletionResponse::new(
                    choice,
                    Usage::default(),
                    Origin::new("fake.chat", "fake", "local"),
                    json!({"adapter":"fake"}),
                )
                .with_finish_reason(FinishReason::Stop);
                run.model_response(ModelTurn::from_response(&reply, &prepared))?;
            }
            AgentRunStep::CallTools { calls } => {
                for call in calls {
                    let PendingToolCall::Execute(call) = call else {
                        return Err("unexpected malformed call".into());
                    };
                    assert_eq!(call.name(), &lookup.name);
                    run.answer(call.answer(ToolResult::success(ToolOutput::text(
                        "case-1: answer available",
                    ))))?;
                }
            }
            AgentRunStep::Done(candidate) => {
                let decision: serde_json::Value = serde_json::from_str(&candidate.output())?;
                assert_eq!(decision["disposition"], "answered");
                println!("Judgment candidate: {decision}");
                // A later runtime Finalizer checks evidence and business state.
                break;
            }
        }
    }
    Ok(())
}

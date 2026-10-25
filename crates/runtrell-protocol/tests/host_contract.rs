// SPDX-License-Identifier: Apache-2.0
use runtrell_protocol::{
    completion::{CompletionResponse, FinishReason, Usage},
    error::{ErrorKind, ErrorReport, ProviderError},
    message::{AssistantContent, Origin},
    run::{AgentRun, AgentRunStep, ModelTurn, policy::TurnPolicy, spec::RunSpec},
};
use serde_json::json;

#[test]
fn removed_turn_limit_is_rejected_instead_of_silently_ignored() {
    assert!(serde_json::from_value::<RunSpec>(json!({"max_turns": 1})).is_err());
}

#[test]
fn opaque_model_continuation_survives_response_run_and_restore() {
    let continuation = json!({"ciphertext":"opaque", "signature":[1,2,3], "unknown":{"x":42}});
    let mut run = AgentRun::new("classify");
    assert!(matches!(
        run.next_step().unwrap(),
        AgentRunStep::CallModel { .. }
    ));
    let reply = CompletionResponse::new(
        vec![AssistantContent::text(r#"{"decision":"review"}"#).with_native(continuation.clone())],
        Usage::default(),
        Origin::new("host.api", "host", "model"),
        json!({"continuation":continuation}),
    )
    .with_finish_reason(FinishReason::Stop);
    run.model_response(ModelTurn::from_policy(&reply, TurnPolicy::default()))
        .unwrap();
    let serialized = serde_json::to_value(&run).unwrap();
    let mut restored: AgentRun = serde_json::from_value(serialized).unwrap();
    let AgentRunStep::Done(candidate) = restored.next_step().unwrap() else {
        panic!("candidate")
    };
    assert_eq!(candidate.content[0].native_item(), Some(&continuation));
    assert_eq!(
        candidate.completion_calls[0].raw["continuation"],
        continuation
    );
    assert_eq!(candidate.usage.input_tokens, None);
    assert_eq!(candidate.output(), r#"{"decision":"review"}"#);
}

#[test]
fn host_failure_metadata_and_retry_verdict_survive_serde_and_prompt_error() {
    let mut report = ErrorReport::new(ErrorKind::Http, "connection lost")
        .with_request_id("r1")
        .with_retryable(true);
    report.provider_response =
        Some(json!({"body":"unparsed", "headers":[["x", [128,255]]], "job":"j1"}));
    let back: ErrorReport = serde_json::from_value(serde_json::to_value(&report).unwrap()).unwrap();
    let error = ProviderError::Relayed(Box::new(back));
    assert_eq!(error.report(), report);
    assert!(error.is_retryable());
    let prompt_error: runtrell_protocol::run::response::PromptError = error.into();
    let runtrell_protocol::run::response::PromptError::Report(back) = prompt_error else {
        panic!("report")
    };
    assert_eq!(*back, report);
}

#[test]
fn structured_output_is_presence_checking_and_not_full_schema_validation() {
    let schema = json!({"type":"object", "properties":{"n":{"type":"integer"}}, "required":["n"]});
    assert!(!runtrell_protocol::structured_output::text_satisfies_schema(Some(&schema), "{}"));
    assert!(runtrell_protocol::structured_output::text_satisfies_schema(
        Some(&schema),
        r#"{"n":"text"}"#
    ));
    assert!(
        !runtrell_protocol::structured_output::text_satisfies_schema(Some(&schema), "{partial")
    );
}

#[test]
fn partial_stream_failure_keeps_content_and_cannot_be_a_successful_turn() {
    use runtrell_protocol::message::{Message, StopReason};
    let mut response = CompletionResponse::new(
        vec![AssistantContent::text("answer")],
        Usage::default(),
        Origin::new("test", "test", "test"),
        json!(null),
    );
    response.error = Some("mid-stream failure".into());
    let Some(Message::Assistant(turn)) = response.message() else {
        panic!("partial content remains")
    };
    assert_eq!(turn.content[0], AssistantContent::text("answer"));
    assert!(
        matches!(&turn.stop, Some(StopReason::Error(error)) if error.contains("mid-stream failure"))
    );
    assert!(
        runtrell_protocol::message::turn_failure(
            &turn.content,
            turn.stop.as_ref(),
            response.finish_reason().as_ref()
        )
        .is_some()
    );
}

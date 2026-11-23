// SPDX-License-Identifier: Apache-2.0
#![cfg(any(feature = "openai", feature = "gemini", feature = "openrouter"))]
use crate::*;
use runtrell::*;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
#[cfg(feature = "gemini")]
mod image_contracts {
    type Images = GeminiImages;
    type InputModel = Gemini;
    const MODEL_KIND: &str = "gemini";
    const OPENAI: bool = false;
    fn set_matrix_fixture_endpoint(provider: &mut Images, endpoint: String) {
        provider.0.config.endpoint = endpoint;
    }
    fn fixture_transport(provider: &mut Images) -> &mut crate::adapter::Adapter {
        &mut provider.0
    }
    include!("support/images.rs");
}
#[cfg(feature = "openai")]
mod openai_image_contracts {
    type Images = OpenAiImages;
    type InputModel = OpenAi;
    const MODEL_KIND: &str = "openai";
    const OPENAI: bool = true;
    fn set_matrix_fixture_endpoint(provider: &mut Images, endpoint: String) {
        provider.set_fixture_endpoint(endpoint);
    }
    fn fixture_transport(provider: &mut Images) -> &mut crate::adapter::Adapter {
        &mut provider.adapter
    }
    include!("support/images.rs");
    include!("support/openai_images.rs");
    include!("support/img26.rs");
}
#[cfg(feature = "gemini")]
#[path = "support/attachment_window.rs"]
mod attachment_window;
#[cfg(feature = "gemini")]
#[path = "support/completion.rs"]
mod completion_contracts;
#[path = "support/documents.rs"]
mod document_contracts;
#[cfg(feature = "openai")]
#[path = "support/media_replay.rs"]
mod media_replay;
#[path = "support/outcomes.rs"]
mod outcomes;
#[path = "support/still_images.rs"]
mod still_images;
#[path = "support/view_images.rs"]
mod view_images;

async fn server(
    status: u16,
    headers: &str,
    body: Value,
) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    server_at_least(status, headers, body, 0).await
}
async fn server_at_least(
    status: u16,
    headers: &str,
    body: Value,
    minimum: usize,
) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let body = body.to_string();
    raw_server_at_least(format!(
        "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}", body.len()
    ),minimum).await
}
async fn raw_server(response: String) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    raw_server_at_least(response, 0).await
}
async fn raw_server_at_least(
    response: String,
    minimum: usize,
) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    raw_server_replies(vec![response], minimum).await
}
#[cfg(feature = "openai")]
async fn raw_server_with_replies(
    replies: Vec<Value>,
    minimum: usize,
) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    raw_server_replies(replies.into_iter().map(|body| {
        let body = body.to_string();
        format!("HTTP/1.1 200 Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
    }).collect(), minimum).await
}
async fn raw_server_replies(
    responses: Vec<String>,
    minimum: usize,
) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/test", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        while let Ok(Ok((mut stream, _))) = tokio::time::timeout(
            if requests.is_empty() || requests.len() < minimum {
                Duration::from_secs(5)
            } else {
                Duration::from_millis(250)
            },
            listener.accept(),
        )
        .await
        {
            let mut bytes = Vec::new();
            loop {
                let mut buf = [0; 4096];
                let n = stream.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                bytes.extend_from_slice(&buf[..n]);
                if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                    let len: usize = head
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .map_or(0, |value| value.parse().unwrap());
                    if bytes.len() >= end + 4 + len {
                        break;
                    }
                }
            }
            requests.push(String::from_utf8(bytes).unwrap());
            let response = &responses[(requests.len() - 1).min(responses.len() - 1)];
            let _ = stream.write_all(response.as_bytes()).await;
        }
        requests
    });
    (url, task)
}
#[cfg(feature = "openai")]
async fn sequence_server(
    responses: Vec<(u16, &'static str, Value)>,
) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/test", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for (status, headers, body) in responses {
            let (mut stream, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut bytes = Vec::new();
            loop {
                let mut buf = [0; 4096];
                let n = stream.read(&mut buf).await.unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buf[..n]);
                if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                    let len = head
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .map(|length| length.parse::<usize>().unwrap())
                        .unwrap_or_else(|| {
                            assert!(head.starts_with("get "));
                            0
                        });
                    if bytes.len() >= end + 4 + len {
                        break;
                    }
                }
            }
            requests.push(String::from_utf8(bytes).unwrap());
            let body = body.to_string();
            let response = format!(
                "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        }
        requests
    });
    (url, task)
}
fn config(endpoint: String) -> Config {
    Config {
        endpoint,
        key: ApiKey::new("TEST-KEY-DO-NOT-PRINT"),
        capabilities: Capabilities {
            limits: ModelLimits {
                model: "host-model".into(),
                context: 8192,
                max_output: 1024,
                min_max_output_tokens: 1,
                price: None,
                timeout: 2_000,
            },
            json_schema: true,
            tool_calling: true,
            parallel_tool_calls: true,
            images: ImageCapabilities {
                input: true,
                ..Default::default()
            },
            documents: DocumentCapabilities::default(),
            counters: CounterSupport {
                input: true,
                cached: true,
                output: true,
                reasoning: true,
                image_input: true,
                image_output: true,
                cached_image_input: true,
            },
        },
        retry_policy: RetryPolicy {
            max_attempts: 3,
            backoff: 1_000,
            max_elapsed: 60_000,
        },
        run: RunHandle {
            tenant: "tenant".into(),
            id: "run-a".into(),
        },
        session: Some("session-a".into()),
        input_token_upper_bound: 100,
        max_request_bytes: 65536,
        max_response_bytes: 65536,
        image_limits: ImageLimits::default(),
        document_limits: DocumentLimits::default(),
        attachment_resolver: None,
    }
}
fn context() -> ToolContext {
    ToolContext {
        managed_send: None,
        recovery_proof: None,
        claim: Claim {
            run: config(String::new()).run,
            worker: "worker".into(),
            generation: 1,
            lease_until: u64::MAX,
        },
        principal: "host".into(),
        operation: "model:0".into(),
        attempt: 1,
        deadline: None,
        permission: "model".into(),
        account_binding: "host".into(),
    }
}
fn request() -> ModelRequest {
    ModelRequest {
        attachments: vec![],
        model: "host-model".into(),
        messages: vec![Message::text("user", "hello")],
        tools: vec![],
        max_output: 128,
        metadata: Value::Null,
        images: vec![],
    }
}
fn text(request: ModelRequest) -> PreparedModelRequest {
    PreparedModelRequest::text(request).unwrap()
}
#[cfg(feature = "openai")]
#[tokio::test]
async fn provider_http_error_exposes_safe_typed_diagnostic() {
    let secret = "sk-SYNTHETIC-NEVER-EXPOSE";
    let (url, server) = server(
        400,
        "",
        json!({"error": {
            "type": "invalid_request_error", "code": "invalid_parameter", "param": "svg.model",
            "message": format!("bad request {secret}")
        }}),
    )
    .await;
    let model = OpenAi::new(config(url)).unwrap();
    let result = model.invoke(&context(), text(request())).await.unwrap();
    let diagnostic = result.provider_diagnostic().expect("provider diagnostic");
    assert_eq!(diagnostic.phase, ProviderFailurePhase::HttpError);
    assert_eq!(diagnostic.http_status, Some(400));
    assert_eq!(
        diagnostic.provider_type.as_deref(),
        Some("invalid_request_error")
    );
    assert_eq!(
        diagnostic.provider_code.as_deref(),
        Some("invalid_parameter")
    );
    assert_eq!(diagnostic.provider_param.as_deref(), Some("svg.model"));
    assert!(!format!("{result:?}").contains(secret));
    assert!(!serde_json::to_string(&result).unwrap().contains(secret));
    assert_eq!(server.await.unwrap().len(), 1);
}
#[cfg(feature = "openai")]
#[tokio::test]
async fn provider_diagnostic_distinguishes_transport_and_effective_status() {
    let (url, server) = server(
        400,
        "Retry-After: 7\r\n",
        json!({
            "error": {"code": 429, "type": "rate_limit_error", "param": "svg.model"}
        }),
    )
    .await;
    let result = OpenAi::new(config(url))
        .unwrap()
        .invoke(&context(), text(request()))
        .await
        .unwrap();
    let diagnostic = result.provider_diagnostic().unwrap();
    assert_eq!(diagnostic.http_status, Some(400));
    assert_eq!(diagnostic.provider_code.as_deref(), Some("429"));
    let ExternalResult::Failed {
        code, retry_after, ..
    } = result
    else {
        panic!("failure")
    };
    let failure: Failure = serde_json::from_str(&code).unwrap();
    assert_eq!(failure.status, 429);
    assert_eq!(failure.class, ErrorClass::RateLimit);
    assert_eq!(
        retry_after, None,
        "transport 400 must never authorize a retry"
    );
    assert_eq!(server.await.unwrap().len(), 1);
}
#[cfg(feature = "openai")]
#[tokio::test]
async fn only_documented_openai_overload_and_slow_down_enable_runtime_retry() {
    for (status, error_type, error_code, header, expected_delay) in [
        (
            503,
            "service_unavailable_error",
            "server_is_overloaded",
            "Retry-After: 6\r\n",
            Some(6_000),
        ),
        (
            429,
            "rate_limit_error",
            "slow_down",
            "Retry-After: 2\r\n",
            Some(2_000),
        ),
        (
            503,
            "service_unavailable_error",
            "server_is_overloaded",
            "",
            Some(0),
        ),
        (
            503,
            "service_unavailable_error",
            "server_is_overloaded",
            "Retry-After: 18446744073709551615\r\n",
            Some(u64::MAX),
        ),
        (
            503,
            "service_unavailable_error",
            "server_is_overloaded",
            "Retry-After: 18446744073709551616\r\n",
            Some(u64::MAX),
        ),
        (
            400,
            "rate_limit_error",
            "slow_down",
            "Retry-After: 3\r\n",
            None,
        ),
        (
            429,
            "rate_limit_error",
            "insufficient_quota",
            "Retry-After: 3\r\n",
            None,
        ),
        (
            429,
            "rate_limit_error",
            "rate_limit_exceeded",
            "Retry-After: 3\r\n",
            None,
        ),
        (
            503,
            "service_unavailable_error",
            "other_failure",
            "Retry-After: 3\r\n",
            None,
        ),
    ] {
        let (url, server) = server(
            status,
            header,
            json!({
                "error": {"type": error_type, "code": error_code}
            }),
        )
        .await;
        let result = OpenAi::new(config(url))
            .unwrap()
            .invoke(&context(), text(request()))
            .await
            .unwrap();
        let ExternalResult::Failed {
            code,
            retry_after,
            usage,
            ..
        } = result
        else {
            panic!("saved failure")
        };
        assert_eq!(retry_after, expected_delay, "HTTP {status} {error_code}");
        if status == 503 {
            assert!(matches!(usage.outcome(), UsageOutcome::Unresolved { .. }));
        } else {
            assert_eq!(
                usage.outcome(),
                UsageOutcome::NonBillable(NonBillableReason::RejectedBeforeExecution)
            );
        }
        let failure: Failure = serde_json::from_str(&code).unwrap();
        assert_eq!(
            failure.class == ErrorClass::Retryable,
            expected_delay.is_some()
        );
        assert_eq!(server.await.unwrap().len(), 1, "adapter must send once");
    }
}

#[cfg(feature = "openai")]
#[test]
fn model_retry_policy_is_host_selected_and_rejects_zero_bounds() {
    let endpoint = "http://127.0.0.1:1/test".to_owned();
    let default = OpenAi::new(config(endpoint.clone())).unwrap();
    assert_eq!(default.retry_policy(), RetryPolicy::default());
    let mut custom = config(endpoint.clone());
    custom.retry_policy = RetryPolicy {
        max_attempts: 2,
        backoff: 200,
        max_elapsed: 4_000,
    };
    assert_eq!(OpenAi::new(custom).unwrap().retry_policy().max_attempts, 2);
    for invalid in [
        RetryPolicy {
            max_attempts: 0,
            ..RetryPolicy::default()
        },
        RetryPolicy {
            backoff: 0,
            ..RetryPolicy::default()
        },
        RetryPolicy {
            max_elapsed: 0,
            ..RetryPolicy::default()
        },
    ] {
        let mut config = config(endpoint.clone());
        config.retry_policy = invalid;
        assert!(OpenAi::new(config).is_err());
    }
}

#[cfg(feature = "openai")]
#[tokio::test]
async fn overload_http_retry_uses_new_runtime_attempt_and_keeps_unknown_charge() {
    use runtrell::memory::MemoryStore;
    use runtrell_testkit::{
        BusinessTool, DecisionFinalizer, DecisionValidator, FakePolicy, TestClock,
    };
    use std::sync::Arc;
    let overload =
        json!({"error":{"type":"service_unavailable_error","code":"server_is_overloaded"}});
    let success = json!({"id":"resp-1","status":"completed","output":[{"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"{\"decision\":\"answered\"}"}]}],"usage":{"input_tokens":3,"output_tokens":1,"total_tokens":4,"input_tokens_details":{"cached_tokens":0,"image_tokens":0,"cached_image_tokens":0},"output_tokens_details":{"reasoning_tokens":0,"image_tokens":0}}});
    let (url, server) = sequence_server(vec![
        (503, "Retry-After: 6\r\n", overload),
        (200, "", success),
    ])
    .await;
    let store = Arc::new(MemoryStore::default());
    let account = store
        .create_account_with_policy(
            "tenant",
            Amount::new(10_000, 10_000),
            None,
            AccountPolicy {
                continue_with_unknown: true,
                ..AccountPolicy::default()
            },
        )
        .await
        .unwrap();
    let run = store
        .start(RunSpec::new(
            "tenant",
            "http-overload",
            account.clone(),
            "answer",
        ))
        .await
        .unwrap();
    let base = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let claim = store.claim(&run, "worker", base, 100_000).await.unwrap();
    let mut config = config(url);
    config.run = run.clone();
    config.session = None;
    config.capabilities.limits.price = Some(Price {
        model: "host-model".into(),
        input: 1_000_000,
        cached: 0,
        output: 1_000_000,
        reasoning: 1_000_000,
        image_input: 1_000_000,
        image_output: 1_000_000,
        cached_image_input: 0,
    });
    let clock = TestClock::default();
    clock.set(base);
    let runtime = Runtime {
        store: store.clone(),
        model: OpenAi::new(config).unwrap(),
        tools: Vec::<BusinessTool>::new(),
        policy: FakePolicy::default(),
        clock: clock.clone(),
        validator: Arc::new(DecisionValidator::default()),
        finalizer: Arc::new(DecisionFinalizer),
    };
    assert!(matches!(
        runtime.drive(&claim).await.unwrap().state,
        RunState::Waiting(_)
    ));
    let old = store.operations(&run).await.unwrap().remove(0);
    assert!(old.unknown_usage);
    assert!(
        matches!(
            old.result,
            Some(ExternalResult::Failed {
                retry_after: Some(6_000),
                ..
            })
        ),
        "actual saved result: {:?}",
        old.result
    );
    clock.set(base + 5_999);
    assert!(matches!(
        runtime.drive(&claim).await.unwrap().state,
        RunState::Waiting(_)
    ));
    clock.set(base + 6_000);
    assert_eq!(
        runtime.drive(&claim).await.unwrap().state,
        RunState::Terminal(Terminal::Succeeded)
    );
    let attempts = store.operations(&run).await.unwrap();
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0].request.attempt, 1);
    assert_eq!(attempts[1].request.attempt, 2);
    assert_eq!(store.account(&account).await.unwrap().unknown, 1);
    assert_eq!(server.await.unwrap().len(), 2);
}

#[cfg(feature = "openai")]
#[tokio::test]
async fn repeated_overload_http_stops_after_three_attempts_with_all_unknowns_held() {
    use runtrell::memory::MemoryStore;
    use runtrell_testkit::{
        BusinessTool, DecisionFinalizer, DecisionValidator, FakePolicy, TestClock,
    };
    use std::sync::Arc;
    let overload =
        json!({"error":{"type":"service_unavailable_error","code":"server_is_overloaded"}});
    let (url, server) = sequence_server(vec![
        (503, "Retry-After: 6\r\n", overload.clone()),
        (503, "Retry-After: 6\r\n", overload.clone()),
        (503, "Retry-After: 6\r\n", overload),
    ])
    .await;
    let store = Arc::new(MemoryStore::default());
    let account = store
        .create_account_with_policy(
            "tenant",
            Amount::new(10_000, 10_000),
            None,
            AccountPolicy {
                continue_with_unknown: true,
                ..AccountPolicy::default()
            },
        )
        .await
        .unwrap();
    let run = store
        .start(RunSpec::new(
            "tenant",
            "http-overload-limit",
            account.clone(),
            "answer",
        ))
        .await
        .unwrap();
    let base = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let claim = store.claim(&run, "worker", base, 100_000).await.unwrap();
    let mut config = config(url);
    config.run = run.clone();
    config.session = None;
    config.capabilities.limits.price = Some(Price {
        model: "host-model".into(),
        input: 1_000_000,
        cached: 0,
        output: 1_000_000,
        reasoning: 1_000_000,
        image_input: 1_000_000,
        image_output: 1_000_000,
        cached_image_input: 0,
    });
    let clock = TestClock::default();
    clock.set(base);
    let runtime = Runtime {
        store: store.clone(),
        model: OpenAi::new(config).unwrap(),
        tools: Vec::<BusinessTool>::new(),
        policy: FakePolicy::default(),
        clock: clock.clone(),
        validator: Arc::new(DecisionValidator::default()),
        finalizer: Arc::new(DecisionFinalizer),
    };
    for time in [0, 6_000, 12_000] {
        clock.set(base + time);
        let state = runtime.drive(&claim).await.unwrap().state;
        assert!(
            matches!(state, RunState::Waiting(_)),
            "state at {time}: {state:?}"
        );
    }
    clock.set(base + 18_000);
    assert!(matches!(
        runtime.drive(&claim).await.unwrap().state,
        RunState::Terminal(Terminal::Failed(_))
    ));
    let attempts = store.operations(&run).await.unwrap();
    assert_eq!(attempts.len(), 3);
    assert!(
        attempts
            .iter()
            .all(|op| op.unknown_usage && matches!(op.result, Some(ExternalResult::Failed { .. })))
    );
    assert_eq!(store.account(&account).await.unwrap().unknown, 3);
    assert_eq!(server.await.unwrap().len(), 3);
}
#[cfg(feature = "openai")]
#[tokio::test]
async fn provider_diagnostic_rejects_hostile_fields_and_non_json_body() {
    let secret = "sk-SYNTHETIC-NEVER-EXPOSE";
    let (url, server) = server(
        401,
        "",
        json!({"error": {
            "type": "bad/secret", "code": "X".repeat(129), "param": "svg/model",
            "message": secret,
        }}),
    )
    .await;
    let result = OpenAi::new(config(url))
        .unwrap()
        .invoke(&context(), text(request()))
        .await
        .unwrap();
    let diagnostic = result.provider_diagnostic().unwrap();
    assert_eq!(diagnostic.http_status, Some(401));
    assert_eq!(diagnostic.provider_type, None);
    assert_eq!(diagnostic.provider_code, None);
    assert_eq!(diagnostic.provider_param, None);
    assert!(!format!("{result:?}").contains(secret));
    assert!(!serde_json::to_string(&result).unwrap().contains(secret));
    assert_eq!(server.await.unwrap().len(), 1);
    let raw = "<html>sk-SYNTHETIC-NEVER-EXPOSE</html>";
    let (url, server) = raw_server(format!(
        "HTTP/1.1 500 Error\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{raw}",
        raw.len()
    ))
    .await;
    let result = OpenAi::new(config(url))
        .unwrap()
        .invoke(&context(), text(request()))
        .await
        .unwrap();
    let diagnostic = result.provider_diagnostic().unwrap();
    assert_eq!(diagnostic.http_status, Some(500));
    assert_eq!(diagnostic.phase, ProviderFailurePhase::HttpError);
    assert_eq!(
        (
            diagnostic.provider_type.clone(),
            diagnostic.provider_code.clone(),
            diagnostic.provider_param.clone()
        ),
        (None, None, None)
    );
    assert!(!serde_json::to_string(&result).unwrap().contains(secret));
    assert_eq!(server.await.unwrap().len(), 1);
}
#[cfg(feature = "openai")]
#[tokio::test]
async fn rejected_request_settles_without_reported_counters() {
    let (url, server) = server(
        403,
        "",
        json!({"error":{"type":"invalid_request_error","code":"permission_denied"}}),
    )
    .await;
    let result = OpenAi::new(config(url))
        .unwrap()
        .invoke(&context(), text(request()))
        .await
        .unwrap();
    let ExternalResult::Failed { usage, .. } = result else {
        panic!("expected a definite rejection");
    };
    assert_eq!(usage.amount(), Ok(Amount::default()));
    assert_eq!(server.await.unwrap().len(), 1);
}

#[cfg(feature = "openai")]
#[tokio::test]
async fn provider_success_status_with_invalid_schema_preserves_definite_failure() {
    let (url, server) = server(200, "", json!({})).await;
    let result = OpenAi::new(config(url))
        .unwrap()
        .invoke(&context(), text(request()))
        .await
        .unwrap();
    let diagnostic = result
        .provider_diagnostic()
        .expect("schema decode diagnostic");
    assert_eq!(diagnostic.phase, ProviderFailurePhase::ResponseDecode);
    assert_eq!(diagnostic.send_certainty, ProviderSendCertainty::Sent);
    assert_eq!(diagnostic.http_status, Some(200));
    assert!(matches!(result, ExternalResult::Failed { .. }));
    assert_eq!(server.await.unwrap().len(), 1);
}
#[cfg(feature = "gemini")]
#[tokio::test]
async fn gemini_structured_error_allowlist() {
    let (url, server) = server(429, "", json!({"error": {
        "code": 429, "status": "RESOURCE_EXHAUSTED", "message": "sk-SYNTHETIC-NEVER-EXPOSE",
        "details": [{"@type":"type.googleapis.com/google.rpc.BadRequest",
            "fieldViolations":[{"field":"generationConfig.maxOutputTokens", "description":"secret"}]}]
    }})).await;
    let result = Gemini::new(config(url))
        .unwrap()
        .invoke(&context(), text(request()))
        .await
        .unwrap();
    let diagnostic = result.provider_diagnostic().unwrap();
    assert_eq!(
        diagnostic.provider_type.as_deref(),
        Some("RESOURCE_EXHAUSTED")
    );
    assert_eq!(diagnostic.provider_code.as_deref(), Some("429"));
    assert_eq!(
        diagnostic.provider_param.as_deref(),
        Some("generationConfig.maxOutputTokens")
    );
    assert!(
        !serde_json::to_string(&result)
            .unwrap()
            .contains("sk-SYNTHETIC")
    );
    assert_eq!(server.await.unwrap().len(), 1);
}
#[cfg(feature = "openrouter")]
#[tokio::test]
async fn openrouter_structured_error_allowlist() {
    let (url, server) = server(
        429,
        "",
        json!({"error": {
            "type":"rate_limit_error", "code":"rate_limit", "param":"model",
            "message":"sk-SYNTHETIC-NEVER-EXPOSE", "metadata":{"secret":"hidden"}
        }}),
    )
    .await;
    let result = OpenRouter::new(config(url))
        .unwrap()
        .invoke(&context(), text(request()))
        .await
        .unwrap();
    let diagnostic = result.provider_diagnostic().unwrap();
    assert_eq!(
        diagnostic.provider_type.as_deref(),
        Some("rate_limit_error")
    );
    assert_eq!(diagnostic.provider_code.as_deref(), Some("rate_limit"));
    assert_eq!(diagnostic.provider_param.as_deref(), Some("model"));
    assert!(
        !serde_json::to_string(&result)
            .unwrap()
            .contains("sk-SYNTHETIC")
    );
    assert_eq!(server.await.unwrap().len(), 1);
}
async fn wire_contract<T: ModelPort + std::fmt::Debug>(
    make: fn(Config) -> Result<T>,
    kind: &str,
    response: Value,
) {
    let (url, task) = server(200, "", response).await;
    let adapter = make(config(url)).unwrap();
    assert!(!format!("{adapter:?}").contains("TEST-KEY-DO-NOT-PRINT"));
    let result = adapter.invoke(&context(), text(request())).await.unwrap();
    let ExternalResult::Applied { output, usage, .. } = result else {
        panic!("expected applied");
    };
    assert_eq!(usage.input, Some(12));
    assert_eq!(usage.output, Some(8));
    assert_eq!(usage.cached, Some(3));
    assert_eq!(usage.reasoning, Some(2));
    assert_eq!(usage.total().unwrap(), 20);
    assert_eq!(usage.micro_usd, None);
    let reply: ModelReply = serde_json::from_value(output).unwrap();
    assert_eq!(reply.text, "hello back");
    assert_eq!(reply.metadata["stop_reason"], "completed");
    let sent = task.await.unwrap();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].starts_with("POST /test HTTP/1.1\r\n"));
    let wire: Value = serde_json::from_str(sent[0].split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert!(
        sent[0].to_lowercase().contains(
            if kind == "gemini" {
                "x-goog-api-key: TEST-KEY-DO-NOT-PRINT"
            } else {
                "authorization: Bearer TEST-KEY-DO-NOT-PRINT"
            }
            .to_lowercase()
            .as_str()
        )
    );
    match kind {
        "openai" => {
            assert_eq!(wire["input"][0]["content"][0]["text"], "hello");
            assert_eq!(wire["max_output_tokens"], 128);
        }
        "gemini" => {
            assert_eq!(wire["contents"][0]["parts"][0]["text"], "hello");
            assert_eq!(wire["generationConfig"]["maxOutputTokens"], 128);
        }
        _ => {
            assert_eq!(wire["messages"][0]["content"], "hello");
            assert_eq!(wire["max_tokens"], 128);
        }
    }
}
#[cfg(feature = "openai")]
#[tokio::test]
async fn openai_wire() {
    wire_contract(OpenAi::new,"openai",json!({"id":"resp-1","status":"completed","model":"actual-model","output":[{"type":"message","status":"completed","role":"assistant","content":[{"type":"output_text","text":"hello back"}]}],"usage":{"input_tokens":12,"output_tokens":8,"input_tokens_details":{"cached_tokens":3},"output_tokens_details":{"reasoning_tokens":2}}})).await;
}
#[cfg(feature = "gemini")]
#[tokio::test]
async fn gemini_wire() {
    wire_contract(Gemini::new,"gemini",json!({"responseId":"resp-1","modelVersion":"actual-model","candidates":[{"finishReason":"STOP","content":{"role":"model","parts":[{"text":"hello back"}]}}],"usageMetadata":{"promptTokenCount":12,"candidatesTokenCount":6,"thoughtsTokenCount":2,"cachedContentTokenCount":3}})).await;
}
#[cfg(feature = "openrouter")]
#[tokio::test]
async fn openrouter_wire() {
    wire_contract(OpenRouter::new,"openrouter",json!({"id":"resp-1","model":"actual-model","choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"hello back"}}],"usage":{"prompt_tokens":12,"completion_tokens":8,"prompt_tokens_details":{"cached_tokens":3},"completion_tokens_details":{"reasoning_tokens":2}}})).await;
}

fn response(kind: &str, stop: &str, tool: bool) -> Value {
    match kind {
        "openai" => {
            json!({"id":"response-1","status":if stop == "max_tokens" || stop == "content_filter" {"incomplete"} else {"completed"},"incomplete_details":{"reason":match stop {"max_tokens" => "max_output_tokens", "content_filter" => "content_filter", _ => "unused"}},"output":if tool {json!([{"type":"function_call","status":"completed","call_id":"call-1","name":"lookup","arguments":"{\"q\":1}"}])} else {json!([{"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"answer"}]}])}})
        }
        "gemini" => {
            json!({"responseId":"response-1","candidates":[{"finishReason":match stop {"max_tokens" => "MAX_TOKENS","content_filter" => "SAFETY",_ => "STOP"},"content":{"role":"model","parts":if tool {json!([{"functionCall":{"id":"call-1","name":"lookup","args":{"q":1}},"thoughtSignature":"signed-state"}])} else {json!([{"text":"answer","thoughtSignature":"signed-state"}])}}}]})
        }
        _ => {
            json!({"id":"response-1","choices":[{"finish_reason":match stop {"max_tokens" => "length","content_filter" => "content_filter","tool_call" => "tool_calls",_ => "stop"},"message":{"role":"assistant","content":if tool {Value::Null} else {json!("answer")},"reasoning_details":[{"type":"reasoning.encrypted","data":"signed-state"}],"tool_calls":if tool {json!([{"id":"call-1","type":"function","function":{"name":"lookup","arguments":"{\"q\":1}"}}])} else {json!([])}}}]})
        }
    }
}
fn tool_request() -> ModelRequest {
    let mut request = request();
    request.tools.push(ToolDescriptor { name:"lookup".into(), version:"1".into(), account:"host".into(), permission:"read".into(), capability:"lookup a value".into(), input_schema:json!({"type":"object","properties":{"q":{"type":"integer"}},"required":["q"],"additionalProperties":false}),output_schema:json!({}),side_effect:false,timeout:1,max_output_bytes:100,reserve:Amount::default(),quantities:Default::default(),conflict_scope:None,retry:RetryPolicy {max_attempts:1,backoff:0,max_elapsed:1} });
    request
}
fn applied(result: ExternalResult) -> (ModelReply, Usage) {
    let ExternalResult::Applied { output, usage, .. } = result else {
        panic!("expected applied")
    };
    (serde_json::from_value(output).unwrap(), usage)
}
async fn stops<T: ModelPort>(make: fn(Config) -> Result<T>, kind: &str) {
    for stop in ["completed", "max_tokens", "content_filter", "tool_call"] {
        let (url, task) = server(200, "", response(kind, stop, true)).await;
        let model = make(config(url)).unwrap();
        let (reply, usage) = applied(
            model
                .invoke(&context(), text(tool_request()))
                .await
                .unwrap(),
        );
        let expected = if stop == "completed" && kind != "openrouter" {
            "tool_call"
        } else if stop == "completed" {
            "unknown"
        } else {
            stop
        };
        assert_eq!(reply.metadata["stop_reason"], expected, "{kind}/{stop}");
        assert_eq!(reply.calls.len(), usize::from(expected == "tool_call"));
        assert_eq!(
            reply.complete,
            !matches!(expected, "content_filter" | "unknown")
        );
        assert_eq!(usage.input, None);
        assert_eq!(usage.output, None);
        assert_eq!(reply.metadata["usage_status"], "absent");
        assert_eq!(task.await.unwrap().len(), 1);
    }
    let mut v = response(kind, "max_tokens", true);
    match kind {
        "openai" => {
            v["output"][0]["arguments"] = json!("{");
            v["output"][0]["status"] = json!("incomplete");
        }
        "gemini" => {
            v["candidates"][0]["content"]["parts"][0]["functionCall"]["args"] = Value::Null;
        }
        _ => v["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"] = json!("{"),
    }
    let (url, task) = server(200, "", v).await;
    let model = make(config(url)).unwrap();
    let (reply, _) = applied(
        model
            .invoke(&context(), text(tool_request()))
            .await
            .unwrap(),
    );
    assert_eq!(reply.metadata["stop_reason"], "max_tokens");
    assert!(reply.calls.is_empty());
    assert_eq!(task.await.unwrap().len(), 1);
}
async fn errors<T: ModelPort + std::fmt::Debug>(make: fn(Config) -> Result<T>, kind: &str) {
    let cases = [
        (401, json!({}), ErrorClass::Authentication),
        (400, json!({}), ErrorClass::InvalidRequest),
        (429, json!({}), ErrorClass::RateLimit),
        (
            429,
            json!({"error":{"code":"insufficient_quota"}}),
            ErrorClass::SpendingLimit,
        ),
        (
            429,
            json!({"error":{"status":"RESOURCE_EXHAUSTED"}}),
            ErrorClass::RateLimit,
        ),
        (
            403,
            json!({"error":{"details":[{"reason":"BILLING_DISABLED"}]}}),
            ErrorClass::SpendingLimit,
        ),
        (
            400,
            json!({"error":{"details":[{"reason":"API_KEY_INVALID"}]}}),
            ErrorClass::Authentication,
        ),
        (402, json!({}), ErrorClass::SpendingLimit),
        (
            429,
            json!({"error":{"metadata":{"limit_source":"openrouter_key_limit"}}}),
            ErrorClass::SpendingLimit,
        ),
        (500, json!({}), ErrorClass::Unclassified),
        (200, json!({"error":{"code":429}}), ErrorClass::RateLimit),
    ];
    for (status, mut body, class) in cases {
        body["message"] = json!("echo TEST-KEY-DO-NOT-PRINT");
        let (url, task) = server(status, "Retry-After: 7\r\n", body).await;
        let model = make(config(url)).unwrap();
        let result = model.invoke(&context(), text(request())).await.unwrap();
        let diagnostic = result.provider_diagnostic().expect("HTTP diagnostic");
        assert_eq!(diagnostic.phase, ProviderFailurePhase::HttpError);
        assert_eq!(diagnostic.send_certainty, ProviderSendCertainty::Sent);
        assert_eq!(diagnostic.http_status, Some(status));
        assert!(!format!("{result:?}").contains("TEST-KEY-DO-NOT-PRINT"));
        let ExternalResult::Failed {
            code,
            usage,
            retry_after,
            ..
        } = result
        else {
            panic!("expected failure")
        };
        let failure: Failure = serde_json::from_str(&code).unwrap();
        assert_eq!(failure.class, class);
        assert_eq!(failure.retry_after.as_deref(), Some("7"));
        assert_eq!(
            retry_after, None,
            "header alone does not permit another attempt"
        );
        assert!(matches!(
            usage.outcome(),
            UsageOutcome::NonBillable(_) | UsageOutcome::Unresolved { .. }
        ));
        assert_eq!(task.await.unwrap().len(), 1, "hidden retry");
    }
    let date = httpdate::fmt_http_date(std::time::SystemTime::now() + Duration::from_secs(60));
    let body = if kind == "openai" {
        json!({"error":{"type":"rate_limit_error","code":"slow_down"}})
    } else {
        json!({})
    };
    let (url, task) = server(429, &format!("Retry-After: {date}\r\n"), body).await;
    let model = make(config(url)).unwrap();
    let ExternalResult::Failed {
        retry_after: delay, ..
    } = model.invoke(&context(), text(request())).await.unwrap()
    else {
        panic!("date delay")
    };
    if kind == "openai" {
        assert!((58_000..=60_000).contains(&delay.unwrap()));
    } else {
        assert_eq!(delay, None);
    }
    assert_eq!(task.await.unwrap().len(), 1);
    let (target, target_task) = server(200, "", json!({})).await;
    let (url, task) = server(307, &format!("Location: {target}\r\n"), json!({})).await;
    let model = make(config(url)).unwrap();
    let ExternalResult::Failed { code, .. } =
        model.invoke(&context(), text(request())).await.unwrap()
    else {
        panic!("redirect")
    };
    assert_eq!(
        serde_json::from_str::<Failure>(&code).unwrap().class,
        ErrorClass::Redirect
    );
    assert_eq!(task.await.unwrap().len(), 1);
    assert!(
        target_task.await.unwrap().is_empty(),
        "redirect was followed"
    );
    for (raw, phase, certainty, status) in [
        (
            String::new(),
            ProviderFailurePhase::Send,
            ProviderSendCertainty::Unknown,
            None,
        ),
        (
            "HTTP/1.1 200 OK\r\nContent-Length: 99\r\nConnection: close\r\n\r\n{".into(),
            ProviderFailurePhase::ResponseRead,
            ProviderSendCertainty::Sent,
            Some(200),
        ),
        (
            "HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nnope".into(),
            ProviderFailurePhase::ResponseDecode,
            ProviderSendCertainty::Sent,
            Some(200),
        ),
    ] {
        let (url, task) = raw_server(raw).await;
        let model = make(config(url)).unwrap();
        let result = model.invoke(&context(), text(request())).await.unwrap();
        let diagnostic = result.provider_diagnostic().expect("unknown diagnostic");
        assert_eq!(diagnostic.send_certainty, certainty);
        assert_eq!(diagnostic.phase, phase);
        assert_eq!(diagnostic.http_status, status);
        assert!(matches!(result, ExternalResult::Ambiguous(_)));
        assert_eq!(task.await.unwrap().len(), 1, "disconnect retry");
    }
}
async fn continuation<T: ModelPort>(make: fn(Config) -> Result<T>, kind: &str) {
    let (url, task) = server(200, "", response(kind, "tool_call", true)).await;
    let model = make(config(url.clone())).unwrap();
    let (reply, _) = applied(
        model
            .invoke(&context(), text(tool_request()))
            .await
            .unwrap(),
    );
    let mut next = tool_request();
    next.messages.push(Message {
        role: "provider".into(),
        text: String::new(),
        call_id: None,
        tool: None,
        data: reply.metadata,
    });
    next.messages.push(Message {
        role: "assistant".into(),
        text: String::new(),
        call_id: Some("call-1".into()),
        tool: Some("lookup".into()),
        data: json!({"q":1}),
    });
    next.messages.push(Message {
        role: "tool".into(),
        text: String::new(),
        call_id: Some("call-1".into()),
        tool: Some("lookup".into()),
        data: json!({"value":2}),
    });
    // A new adapter instance resumes the trusted saved state.
    let restored = make(config(url.clone())).unwrap();
    restored
        .invoke(&context(), text(next.clone()))
        .await
        .unwrap();
    for field in [
        "run", "tenant", "session", "endpoint", "model", "wire", "version",
    ] {
        let mut forged = next.clone();
        let state = &mut forged.messages[1].data["continuation"];
        match field {
            "run" => state["run"]["id"] = json!("other"),
            "tenant" => state["run"]["tenant"] = json!("other"),
            "version" => state[field] = json!(2),
            _ => state[field] = json!("other"),
        }
        assert!(
            restored.invoke(&context(), text(forged)).await.is_err(),
            "{field}"
        );
    }
    let mut other_context = context();
    other_context.claim.run.id = "other".into();
    assert_eq!(
        model
            .invoke(&other_context, text(next.clone()))
            .await
            .unwrap_err(),
        Error::PermissionDenied
    );
    let mut other_config = config(url.clone());
    other_config.session = Some("other".into());
    let other = make(other_config).unwrap();
    assert_eq!(
        other
            .invoke(&context(), text(next.clone()))
            .await
            .unwrap_err(),
        Error::PermissionDenied
    );
    let mut other_config = config(url);
    other_config.run = other_context.claim.run.clone();
    let other = make(other_config).unwrap();
    assert_eq!(
        other
            .invoke(&other_context, text(next.clone()))
            .await
            .unwrap_err(),
        Error::PermissionDenied
    );
    assert_eq!(
        model.lookup(&context(), text(next)).await.unwrap(),
        Lookup::Unknown
    );
    assert_eq!(model.recovery(), None);
    assert_eq!(model.retry_policy().max_attempts, 3);
    let sent = task.await.unwrap();
    assert_eq!(
        sent.len(),
        2,
        "refused state must not send; lookup must not resend"
    );
    let body: Value = serde_json::from_str(sent[1].split("\r\n\r\n").nth(1).unwrap()).unwrap();
    match kind {
        "openai" => {
            assert_eq!(body["input"].as_array().unwrap().len(), 3);
            assert_eq!(body["input"][1]["type"], "function_call");
            assert_eq!(body["input"][2]["type"], "function_call_output");
            assert_eq!(body["store"], false);
        }
        "gemini" => {
            assert_eq!(body["contents"].as_array().unwrap().len(), 3);
            assert_eq!(
                body["contents"][1]["parts"][0]["thoughtSignature"],
                "signed-state"
            );
            assert_eq!(
                body["contents"][2]["parts"][0]["functionResponse"]["name"],
                "lookup"
            );
        }
        _ => {
            assert_eq!(body["messages"].as_array().unwrap().len(), 3);
            assert_eq!(
                body["messages"][1]["reasoning_details"][0]["data"],
                "signed-state"
            );
            assert_eq!(body["messages"][2]["tool_call_id"], "call-1");
        }
    }
}
async fn options_and_bounds<T: ModelPort + std::fmt::Debug>(
    make: fn(Config) -> Result<T>,
    kind: &str,
) {
    let (url, task) = server(200, "", response(kind, "completed", false)).await;
    let mut cfg = config(url.clone());
    cfg.capabilities.limits.min_max_output_tokens = 10;
    let model = make(cfg.clone()).unwrap();
    assert_eq!(model.input_upper_bound(&text(request())).unwrap(), 100);
    let mut req = tool_request();
    req.messages
        .insert(0, Message::text("system", "be helpful"));
    req.metadata = json!({"previous_response_id":"untrusted-metadata-never-forwarded","provider_options":{"json_schema":{"type":"object"}}});
    model.invoke(&context(), text(req.clone())).await.unwrap();
    for max in [0, 9, 1025] {
        let mut bad = request();
        bad.max_output = max;
        assert!(model.invoke(&context(), text(bad)).await.is_err());
    }
    let mut bad = request();
    bad.model = "wrong".into();
    assert!(model.invoke(&context(), text(bad)).await.is_err());
    let mut bad = request();
    bad.metadata = json!({"provider_options":{"previous_response_id":"raw-id"}});
    assert!(model.invoke(&context(), text(bad)).await.is_err());
    let mut ctx = context();
    ctx.deadline = Some(0);
    assert_eq!(
        model.invoke(&ctx, text(request())).await.unwrap_err(),
        Error::Deadline
    );
    cfg.capabilities.json_schema = false;
    assert!(
        make(cfg.clone())
            .unwrap()
            .invoke(&context(), text(req.clone()))
            .await
            .is_err()
    );
    cfg.capabilities.json_schema = true;
    cfg.capabilities.tool_calling = false;
    cfg.capabilities.parallel_tool_calls = false;
    assert!(
        make(cfg.clone())
            .unwrap()
            .invoke(&context(), text(tool_request()))
            .await
            .is_err()
    );
    cfg.capabilities.tool_calling = true;
    cfg.max_request_bytes = 1;
    assert!(
        make(cfg.clone())
            .unwrap()
            .invoke(&context(), text(request()))
            .await
            .is_err()
    );
    cfg.max_request_bytes = 65536;
    cfg.max_response_bytes = 1;
    let result = make(cfg.clone())
        .unwrap()
        .invoke(&context(), text(request()))
        .await
        .unwrap();
    let diagnostic = result
        .provider_diagnostic()
        .expect("oversized body diagnostic");
    assert_eq!(diagnostic.phase, ProviderFailurePhase::ResponseByteLimit);
    assert_eq!(diagnostic.send_certainty, ProviderSendCertainty::Sent);
    assert_eq!(diagnostic.http_status, Some(200));
    for endpoint in [
        "https://user:password@example.test/a",
        "https://example.test/a?key=hidden",
        "https://example.test/#fragment",
        "http://example.test/a",
    ] {
        cfg.endpoint = endpoint.into();
        assert!(make(cfg.clone()).is_err());
    }
    cfg.endpoint = url;
    cfg.key = ApiKey::new("invalid\nkey");
    let err = make(cfg).unwrap_err();
    assert!(!format!("{err:?} {err}").contains("invalid\nkey"));
    let sent = task.await.unwrap();
    assert_eq!(sent.len(), 2);
    assert!(!sent[0].contains("untrusted-metadata-never-forwarded"));
    let body: Value = serde_json::from_str(sent[0].split("\r\n\r\n").nth(1).unwrap()).unwrap();
    match kind {
        "openai" => {
            assert_eq!(body["text"]["format"]["type"], "json_schema");
            assert_eq!(body["tools"][0]["name"], "lookup");
        }
        "gemini" => {
            assert_eq!(
                body["generationConfig"]["responseMimeType"],
                "application/json"
            );
            assert_eq!(
                body["tools"][0]["functionDeclarations"][0]["name"],
                "lookup"
            );
            assert_eq!(body["systemInstruction"]["parts"][0]["text"], "be helpful");
        }
        _ => {
            assert_eq!(body["response_format"]["type"], "json_schema");
            assert_eq!(body["tools"][0]["function"]["name"], "lookup");
        }
    }
}
async fn usage_cases<T: ModelPort>(make: fn(Config) -> Result<T>, kind: &str) {
    for (usage, expected_input, expected_output, status) in [
        (Value::Null, None, None, "absent"),
        (json!({}), None, None, "partial"),
        (
            if kind == "gemini" {
                json!({"promptTokenCount":0})
            } else if kind == "openai" {
                json!({"input_tokens":0})
            } else {
                json!({"prompt_tokens":0})
            },
            Some(0),
            None,
            "partial",
        ),
        (
            if kind == "gemini" {
                json!({"promptTokenCount":10,"candidatesTokenCount":5})
            } else if kind == "openai" {
                json!({"input_tokens":10,"output_tokens":5})
            } else {
                json!({"prompt_tokens":10,"completion_tokens":5})
            },
            Some(10),
            if kind == "gemini" { None } else { Some(5) },
            if kind == "gemini" {
                "partial"
            } else {
                "totals_reported"
            },
        ),
        (
            if kind == "gemini" {
                json!({"promptTokenCount":0,"candidatesTokenCount":0,"thoughtsTokenCount":0})
            } else if kind == "openai" {
                json!({"input_tokens":0,"output_tokens":0})
            } else {
                json!({"prompt_tokens":0,"completion_tokens":0})
            },
            Some(0),
            Some(0),
            "totals_reported",
        ),
    ] {
        let mut v = response(kind, "completed", false);
        v[if kind == "gemini" {
            "usageMetadata"
        } else {
            "usage"
        }] = usage;
        let (url, task) = server(200, "", v).await;
        let (reply, u) = applied(
            make(config(url))
                .unwrap()
                .invoke(&context(), text(request()))
                .await
                .unwrap(),
        );
        assert_eq!(u.input, expected_input);
        assert_eq!(u.output, expected_output);
        assert_eq!(u.cached, None);
        assert_eq!(
            u.reasoning,
            None.or(if kind == "gemini" && expected_output == Some(0) {
                Some(0)
            } else {
                None
            })
        );
        assert_eq!(u.image_input, None);
        assert_eq!(u.image_output, None);
        assert_eq!(u.micro_usd, None);
        assert_eq!(reply.metadata["usage_status"], status);
        assert_eq!(task.await.unwrap().len(), 1);
    }
    let mut v = response(kind, "completed", false);
    v[if kind == "gemini" {
        "usageMetadata"
    } else {
        "usage"
    }] = match kind {
        "gemini" => {
            json!({"promptTokenCount":12,"cachedContentTokenCount":4,"candidatesTokenCount":6,"thoughtsTokenCount":2,"promptTokensDetails":[{"modality":"IMAGE","tokenCount":3}],"candidatesTokensDetails":[{"modality":"IMAGE","tokenCount":2}],"cacheTokensDetails":[{"modality":"IMAGE","tokenCount":1}]})
        }
        "openai" => {
            json!({"input_tokens":12,"output_tokens":8,"input_tokens_details":{"cached_tokens":4,"image_tokens":3,"cached_image_tokens":1},"output_tokens_details":{"reasoning_tokens":2,"image_tokens":2}})
        }
        _ => {
            json!({"prompt_tokens":12,"completion_tokens":8,"prompt_tokens_details":{"cached_tokens":4,"image_tokens":3,"cached_image_tokens":1},"completion_tokens_details":{"reasoning_tokens":2,"image_tokens":2}})
        }
    };
    let (url, task) = server(200, "", v).await;
    let (_, u) = applied(
        make(config(url))
            .unwrap()
            .invoke(&context(), text(request()))
            .await
            .unwrap(),
    );
    assert_eq!(u.total().unwrap(), 20);
    assert_eq!(u.image_input, Some(3));
    assert_eq!(u.image_output, Some(2));
    assert_eq!(u.cached_image_input, Some(1));
    assert_eq!(task.await.unwrap().len(), 1);
}
macro_rules! suites {
    ($module:ident, $feature:literal, $adapter:ident, $kind:literal) => {
        #[cfg(feature=$feature)]
        mod $module {
            use super::*;
            #[tokio::test]
            async fn stop_and_partial_calls() {
                stops($adapter::new, $kind).await;
            }
            #[tokio::test]
            async fn error_retry_redirect_disconnect() {
                errors($adapter::new, $kind).await;
            }
            #[tokio::test]
            async fn run_session_continuation() {
                continuation($adapter::new, $kind).await;
            }
            #[tokio::test]
            async fn options_and_limits() {
                options_and_bounds($adapter::new, $kind).await;
            }
            #[tokio::test]
            async fn missing_partial_image_usage() {
                usage_cases($adapter::new, $kind).await;
            }
        }
    };
}
suites!(openai, "openai", OpenAi, "openai");
suites!(gemini, "gemini", Gemini, "gemini");
suites!(openrouter, "openrouter", OpenRouter, "openrouter");

async fn host_price<T: ModelPort>(make: fn(Config) -> Result<T>, kind: &str) {
    for discounted in [false, true] {
        let mut v = response(kind, "completed", false);
        v[if kind == "gemini" {
            "usageMetadata"
        } else {
            "usage"
        }] = match kind {
            "gemini" => {
                json!({"promptTokenCount":12,"candidatesTokenCount":6,"thoughtsTokenCount":2})
            }
            "openai" => json!({"input_tokens":12,"output_tokens":8}),
            _ => json!({"prompt_tokens":12,"completion_tokens":8}),
        };
        let (url, task) = server(200, "", v).await;
        let mut cfg = config(url);
        let rate = 1_000_000;
        let price = Price {
            model: "host-model".into(),
            input: rate,
            cached: if discounted { 0 } else { rate },
            output: rate,
            reasoning: rate,
            image_input: rate,
            image_output: rate,
            cached_image_input: rate,
        };
        cfg.capabilities.limits.price = Some(price.clone());
        let (_, usage) = applied(
            make(cfg)
                .unwrap()
                .invoke(&context(), text(request()))
                .await
                .unwrap(),
        );
        assert_eq!(usage.micro_usd, if discounted { None } else { Some(20) });
        let mut dispatch = DispatchRequest::new(
            "priced",
            OperationKind::Model,
            "host-model",
            Value::Null,
            Amount::new(100, 100),
        );
        dispatch.price = Some(price);
        assert_eq!(
            dispatch.settlement_amount(&usage).unwrap(),
            if discounted {
                None
            } else {
                Some(Amount::new(20, 20))
            }
        );
        assert_eq!(task.await.unwrap().len(), 1);
    }
}
#[cfg(feature = "openai")]
#[tokio::test]
async fn openai_host_price_settlement() {
    host_price(OpenAi::new, "openai").await;
}
#[cfg(feature = "gemini")]
#[tokio::test]
async fn gemini_host_price_settlement() {
    host_price(Gemini::new, "gemini").await;
}
#[cfg(feature = "openrouter")]
#[tokio::test]
async fn openrouter_host_price_settlement() {
    host_price(OpenRouter::new, "openrouter").await;
}

async fn failure_usage<T: ModelPort>(make: fn(Config) -> Result<T>) {
    let body = json!({"error":{"code":429},"usage":{"input_tokens":12,"output_tokens":8,"prompt_tokens":12,"completion_tokens":8},"usageMetadata":{"promptTokenCount":12,"candidatesTokenCount":6,"thoughtsTokenCount":2}});
    let (url, task) = server(429, "", body).await;
    let ExternalResult::Failed { usage, .. } = make(config(url))
        .unwrap()
        .invoke(&context(), text(request()))
        .await
        .unwrap()
    else {
        panic!("expected failure")
    };
    assert_eq!(usage.input, Some(12));
    assert_eq!(usage.output, Some(8));
    assert_eq!(usage.cached, None);
    assert_eq!(usage.micro_usd, None);
    assert_eq!(task.await.unwrap().len(), 1);
}
#[cfg(feature = "openai")]
#[tokio::test]
async fn openai_failure_preserves_usage() {
    failure_usage(OpenAi::new).await;
}
#[cfg(feature = "gemini")]
#[tokio::test]
async fn gemini_failure_preserves_usage() {
    failure_usage(Gemini::new).await;
}
#[cfg(feature = "openrouter")]
#[tokio::test]
async fn openrouter_failure_preserves_usage() {
    failure_usage(OpenRouter::new).await;
}

fn named_request(names: &[&str]) -> ModelRequest {
    let mut req = tool_request();
    let descriptor = req.tools[0].clone();
    req.tools = names
        .iter()
        .map(|name| {
            let mut tool = descriptor.clone();
            tool.name = (*name).into();
            tool
        })
        .collect();
    req
}
fn tool_wire_name(name: &str, kind: &str) -> String {
    if kind == "gemini"
        || (name.len() <= 64
            && !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
    {
        name.into()
    } else {
        digest(&json!(name))
    }
}
fn named_response(kind: &str, name: &str) -> Value {
    let mut v = response(kind, "tool_call", true);
    match kind {
        "openai" => v["output"][0]["name"] = json!(name),
        "gemini" => v["candidates"][0]["content"]["parts"][0]["functionCall"]["name"] = json!(name),
        _ => v["choices"][0]["message"]["tool_calls"][0]["function"]["name"] = json!(name),
    }
    v
}
async fn tool_name_contract<T: ModelPort>(make: fn(Config) -> Result<T>, kind: &str) {
    let long = "x".repeat(65);
    let names = ["files.list", "files_list", long.as_str(), "x", "a"];
    let wire_name = tool_wire_name("files.list", kind);
    let (url, task) = server(200, "", named_response(kind, &wire_name)).await;
    let model = make(config(url.clone())).unwrap();
    let mut req = named_request(&names);
    req.messages.push(Message {
        role: "assistant".into(),
        text: String::new(),
        call_id: Some("old-call".into()),
        tool: Some("files.list".into()),
        data: json!({"q":1}),
    });
    req.messages.push(Message {
        role: "tool".into(),
        text: String::new(),
        call_id: Some("old-call".into()),
        tool: Some("files.list".into()),
        data: json!({"value":2}),
    });
    let identity = digest(&serde_json::to_value(&req).unwrap());
    let (reply, _) = applied(model.invoke(&context(), text(req.clone())).await.unwrap());
    assert_eq!(reply.calls.len(), 1);
    assert_eq!(reply.calls[0].name, "files.list");
    assert_eq!(reply.calls[0].id, "call-1");
    assert_eq!(digest(&serde_json::to_value(&req).unwrap()), identity);
    // Reconstruct an adapter and replay its saved provider state.
    let mut next = req.clone();
    next.tools.reverse();
    next.messages.push(Message {
        role: "provider".into(),
        text: String::new(),
        call_id: None,
        tool: None,
        data: reply.metadata,
    });
    next.messages.push(Message {
        role: "assistant".into(),
        text: String::new(),
        call_id: Some("call-1".into()),
        tool: Some("files.list".into()),
        data: json!({"q":1}),
    });
    next.messages.push(Message {
        role: "tool".into(),
        text: String::new(),
        call_id: Some("call-1".into()),
        tool: Some("files.list".into()),
        data: json!({"value":2}),
    });
    let (again, _) = applied(
        make(config(url))
            .unwrap()
            .invoke(&context(), text(next))
            .await
            .unwrap(),
    );
    assert_eq!(again.calls[0].name, "files.list");
    let sent = task.await.unwrap();
    assert_eq!(sent.len(), 2);
    for raw in sent {
        let body: Value = serde_json::from_str(raw.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        let definitions = match kind {
            "gemini" => body["tools"][0]["functionDeclarations"].as_array().unwrap(),
            _ => body["tools"].as_array().unwrap(),
        };
        let mut actual = Vec::new();
        for def in definitions {
            let name = if kind == "openrouter" {
                def["function"]["name"].as_str().unwrap()
            } else {
                def["name"].as_str().unwrap()
            };
            if kind != "gemini" {
                assert!(name.len() <= 64);
                assert!(
                    name.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                );
            }
            actual.push(name.to_owned());
        }
        let mut expected: Vec<_> = names.iter().map(|n| tool_wire_name(n, kind)).collect();
        expected.sort();
        actual.sort();
        assert_eq!(actual, expected);
        actual.dedup();
        assert_eq!(actual.len(), names.len());
        match kind {
            "openai" => {
                assert_eq!(body["input"][1]["name"], wire_name);
                if body["input"].as_array().unwrap().len() > 3 {
                    assert_eq!(body["input"][3]["name"], wire_name);
                }
            }
            "gemini" => assert_eq!(
                body["contents"][1]["parts"][0]["functionCall"]["name"],
                "files.list"
            ),
            _ => {
                assert_eq!(
                    body["messages"][1]["tool_calls"][0]["function"]["name"],
                    wire_name
                );
                if body["messages"].as_array().unwrap().len() > 3 {
                    assert_eq!(
                        body["messages"][3]["tool_calls"][0]["function"]["name"],
                        wire_name
                    );
                }
            }
        }
    }
    let (url, task) = server(200, "", named_response(kind, "unknown_wire_name")).await;
    let (reply, _) = applied(
        make(config(url))
            .unwrap()
            .invoke(&context(), text(req))
            .await
            .unwrap(),
    );
    assert!(reply.calls.is_empty());
    assert_eq!(reply.metadata["stop_reason"], "unknown");
    assert_eq!(task.await.unwrap().len(), 1);
    if kind != "gemini" {
        let collision = tool_wire_name("files.list", kind);
        let (url, task) = server(200, "", response(kind, "completed", false)).await;
        let model = make(config(url)).unwrap();
        let req = text(named_request(&["files.list", &collision]));
        assert!(model.input_upper_bound(&req).is_err());
        assert!(model.invoke(&context(), req).await.is_err());
        assert!(task.await.unwrap().is_empty());
    }
}
#[cfg(feature = "openai")]
#[tokio::test]
async fn openai_tool_names() {
    tool_name_contract(OpenAi::new, "openai").await;
}
#[cfg(feature = "openrouter")]
#[tokio::test]
async fn chat_tool_names() {
    tool_name_contract(OpenRouter::new, "openrouter").await;
}
#[cfg(feature = "gemini")]
#[tokio::test]
async fn gemini_tool_names() {
    tool_name_contract(Gemini::new, "gemini").await;
}

#[cfg(feature = "openai")]
#[tokio::test]
async fn bundled_model_requires_reservation_and_settles_once() {
    use runtrell::memory::MemoryStore;
    use runtrell_testkit::{
        BusinessTool, DecisionFinalizer, DecisionValidator, FakePolicy, TestClock,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    // A temporary catalog build can exercise this same caller with a synthetic model.
    let id = std::env::var("RUNTRELL_CATALOG_DEMO_MODEL")
        .unwrap_or_else(|_| "openai/gpt-6.2-sol".into());
    let expect_unknown = std::env::var_os("RUNTRELL_CATALOG_DEMO_UNKNOWN").is_some();
    let wire_id = id.split_once('/').unwrap().1;
    let (url, server) = raw_server_with_replies(vec![
        json!({"object":"response.input_tokens","input_tokens":100}),
        json!({
        "id": "resp-local",
        "model": wire_id,
        "status": "completed",
        "service_tier": "default",
        "output": [{"type": "message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"{\"decision\":\"answered\"}"}]}],
        "usage": {"input_tokens":100,"input_tokens_details":
            {"cached_tokens":20,"cache_write_tokens":0,"cache_write_tokens_details":
             {"ttl_30m_tokens":0,"ttl_12h_tokens":0}},
            "output_tokens":50,"output_tokens_details":{"reasoning_tokens":10},
            "total_tokens":150}
    })], 0).await;
    let store = Arc::new(MemoryStore::default());
    let account = store
        .create_account("tenant", Amount::new(2_000_000, 10_000_000), None)
        .await
        .unwrap();
    let run = store
        .start(RunSpec::new(
            "tenant",
            "catalog-http",
            account.clone(),
            "answer",
        ))
        .await
        .unwrap();
    let base = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let claim = store.claim(&run, "worker", base, 100_000).await.unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let connection = Connection {
        run: run.clone(),
        session: None,
        timeout_ms: 10_000,
        retry_policy: RetryPolicy::default(),
        attachment_resolver: None,
        credentials: Arc::new({
            let calls = calls.clone();
            move |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(ApiKey::new("FIXTURE-KEY"))
            }
        }),
        context_cap: None,
        input_token_cap: None,
        max_output_tokens: Some(64),
        reasoning: Some("low".into()),
    };
    let configured = connect_model(&id, None, connection.clone());
    if expect_unknown {
        assert!(matches!(
            configured,
            Err(ConnectError::Resolve(models::ResolveError::UnknownModel))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(!server.is_finished());
        server.abort();
        return;
    }
    let mut raised = connection.clone();
    raised.max_output_tokens = Some(u64::MAX);
    assert!(matches!(
        connect_model(&id, None, raised),
        Err(ConnectError::Configuration(_))
    ));
    let mut invalid_reasoning = connection;
    invalid_reasoning.reasoning = Some("none".into());
    assert!(matches!(
        connect_model(&id, None, invalid_reasoning),
        Err(ConnectError::Configuration(_))
    ));

    let mut model = configured.unwrap();
    // The endpoint is writable by wire fixtures only. The catalog snapshot and
    // ModelPort accounting path are unchanged.
    model.0.config.endpoint = url;
    assert_eq!(model.limits().max_output, 64);
    let mut request = request();
    request.model = wire_id.into();
    request.max_output = 64;
    let mut unreserved = context();
    unreserved.claim = claim.clone();
    assert!(matches!(
        model.invoke(&unreserved, text(request)).await,
        Err(Error::ReservationDenied)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    let clock = TestClock::default();
    clock.set(base);
    let runtime = Runtime {
        store: store.clone(),
        model,
        tools: Vec::<BusinessTool>::new(),
        policy: FakePolicy::default(),
        clock,
        validator: Arc::new(DecisionValidator::default()),
        finalizer: Arc::new(DecisionFinalizer),
    };
    assert_eq!(
        runtime.drive(&claim).await.unwrap().state,
        RunState::Terminal(Terminal::Succeeded)
    );
    let op = store.operations(&run).await.unwrap().remove(0);
    assert!(op.settled);
    assert!(op.request.pricing.is_some());
    assert_eq!(op.send_count, 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        store.account(&account).await.unwrap().used,
        Amount::new(150, 662)
    );
    store
        .record(&claim, &op.id, op.result.unwrap(), base)
        .await
        .unwrap();
    assert_eq!(
        store.account(&account).await.unwrap().used,
        Amount::new(150, 662)
    );
    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with("POST /test/input_tokens "));
    let payload: Value =
        serde_json::from_str(requests[1].split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(payload["max_output_tokens"], 64);
    assert_eq!(payload["service_tier"], "default");
    assert_eq!(payload["prompt_cache_options"]["ttl"], "30m");
    println!("catalog digest {}", runtime.model.model().catalog_digest());
}

#[cfg(feature = "openai")]
#[tokio::test]
async fn managed_context_cap_counts_final_input_and_blocks_long_context() {
    use runtrell::memory::MemoryStore;
    use runtrell_testkit::{
        BusinessTool, DecisionFinalizer, DecisionValidator, FakePolicy, TestClock,
    };

    for (count, input_only, above_estimate, allowed) in [
        (271_936, false, false, true),
        (271_937, false, false, false),
        (272_000, true, false, true),
        (272_001, true, false, false),
        (10_000, true, true, false),
    ] {
        let (url, server) = raw_server_with_replies(vec![
            json!({"object":"response.input_tokens","input_tokens":count}),
            json!({"id":"resp-cap","model":"gpt-6.2-sol","status":"completed","service_tier":"default",
                "output":[{"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"{\"decision\":\"answered\"}"}]}],
                "usage":{"input_tokens":count,"input_tokens_details":{"cached_tokens":0,"cache_write_tokens":0},
                         "output_tokens":32,"total_tokens":count + 32}}),
        ], 1).await;
        let store = Arc::new(MemoryStore::default());
        let account = store
            .create_account("tenant", Amount::new(272_064, 700_000), None)
            .await
            .unwrap();
        let input = if above_estimate {
            "answer".into()
        } else {
            "a".repeat(272_000)
        };
        let run = store
            .start(RunSpec::new(
                "tenant",
                "bounded-context",
                account.clone(),
                &input,
            ))
            .await
            .unwrap();
        let base = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let claim = store.claim(&run, "worker", base, 100_000).await.unwrap();
        let clock = TestClock::default();
        clock.set(base);
        let mut model = connect_model(
            "openai/gpt-6.2-sol",
            None,
            Connection {
                run,
                session: None,
                credentials: Arc::new(|_| Ok(ApiKey::new("FIXTURE-KEY"))),
                attachment_resolver: None,
                retry_policy: RetryPolicy::default(),
                timeout_ms: 10_000,
                context_cap: (!input_only).then_some(272_000),
                input_token_cap: input_only.then_some(272_000),
                max_output_tokens: Some(64),
                reasoning: Some("low".into()),
            },
        )
        .unwrap();
        model.0.config.endpoint = url;
        let runtime = Runtime {
            store: store.clone(),
            model,
            tools: Vec::<BusinessTool>::new(),
            policy: FakePolicy::default(),
            clock,
            validator: Arc::new(DecisionValidator::default()),
            finalizer: Arc::new(DecisionFinalizer),
        };
        let result = runtime.drive(&claim).await;
        let calls = server.await.unwrap();
        assert!(calls[0].starts_with("POST /test/input_tokens "));
        assert_eq!(calls.len(), if allowed { 2 } else { 1 });
        let usage = store.account(&account).await.unwrap().used;
        if allowed {
            assert_eq!(
                result.unwrap().state,
                RunState::Terminal(Terminal::Succeeded)
            );
            let counted: Value =
                serde_json::from_str(calls[0].split_once("\r\n\r\n").unwrap().1).unwrap();
            let sent: Value =
                serde_json::from_str(calls[1].split_once("\r\n\r\n").unwrap().1).unwrap();
            assert_eq!(counted["input"], sent["input"]);
            assert_eq!(sent["max_output_tokens"], 64);
            assert_eq!(usage, Amount::new(count + 32, count * 2 + 32 * 10));
        } else {
            assert_eq!(usage, Amount::new(0, 0));
            let op = store.operations(&claim.run).await.unwrap().remove(0);
            assert!(op.settled);
            assert!(
                matches!(op.result, Some(ExternalResult::Failed { code, .. }) if code == if above_estimate { "INPUT_TOKEN_ESTIMATE_EXCEEDED" } else { "CONTEXT_CAP_EXCEEDED" })
            );
        }
    }
}

#[cfg(feature = "openrouter")]
#[tokio::test]
async fn managed_openrouter_requires_the_bound_payer_and_checks_the_charge() {
    use runtrell::memory::MemoryStore;
    use runtrell_testkit::{
        BusinessTool, DecisionFinalizer, DecisionValidator, FakePolicy, TestClock,
    };
    let id = "openrouter/meta-llama/llama-3.1-8b-instruct";
    for (binding, payer, is_byok, cost, upstream, expected) in [
        (
            "fp8-standard",
            OpenRouterPayer::Credits,
            false,
            0.0001807,
            None,
            Some(181),
        ),
        (
            "deepinfra-standard",
            OpenRouterPayer::Credits,
            false,
            0.000024,
            None,
            Some(24),
        ),
        (
            "deepinfra-standard",
            OpenRouterPayer::Credits,
            false,
            0.000056,
            None,
            None,
        ),
        (
            "deepinfra-standard",
            OpenRouterPayer::Credits,
            true,
            0.000024,
            None,
            None,
        ),
        (
            "deepinfra-standard",
            OpenRouterPayer::Byok,
            false,
            0.000024,
            None,
            None,
        ),
        (
            "deepinfra-byok",
            OpenRouterPayer::Byok,
            true,
            0.0,
            Some(0.000024),
            Some(24),
        ),
        (
            "deepinfra-byok",
            OpenRouterPayer::Byok,
            true,
            0.0000012,
            Some(0.000024),
            Some(26),
        ),
        (
            "deepinfra-byok",
            OpenRouterPayer::Byok,
            true,
            0.0000200,
            Some(0.000024),
            None,
        ),
        (
            "deepinfra-byok",
            OpenRouterPayer::Byok,
            true,
            0.0,
            None,
            None,
        ),
    ] {
        let multi_route = binding == "fp8-standard";
        let provider = if multi_route {
            "Cloudflare"
        } else {
            "DeepInfra"
        };
        let (url, http) = server(200, "", json!({
            "id":"gen-fixture", "model":"meta-llama/llama-3.1-8b-instruct", "provider":provider, "service_tier":"default",
            "choices":[{"message":{"role":"assistant","content":"{\"decision\":\"answered\"}"},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":1000,"completion_tokens":100,"total_tokens":1100,"is_byok":is_byok,"cost":cost,
                "cost_details":{"upstream_inference_cost":upstream}}
        })).await;
        let store = Arc::new(MemoryStore::default());
        let account = store
            .create_account("tenant", Amount::new(200_000, 100_000), None)
            .await
            .unwrap();
        let run = store
            .start(RunSpec::new("tenant", "router", account.clone(), "answer"))
            .await
            .unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let claim = store.claim(&run, "worker", now, 100_000).await.unwrap();
        let clock = TestClock::default();
        clock.set(now);
        let mut model = connect_model(
            id,
            Some(binding),
            Connection {
                run: run.clone(),
                session: None,
                credentials: Arc::new(move |_| Ok(ApiKey::openrouter("FIXTURE-KEY", payer))),
                attachment_resolver: None,
                retry_policy: RetryPolicy::default(),
                timeout_ms: 10_000,
                context_cap: None,
                input_token_cap: None,
                max_output_tokens: Some(128),
                reasoning: None,
            },
        )
        .unwrap();
        model.0.config.endpoint = url;
        let runtime = Runtime {
            store: store.clone(),
            model,
            tools: Vec::<BusinessTool>::new(),
            policy: FakePolicy::default(),
            clock,
            validator: Arc::new(DecisionValidator::default()),
            finalizer: Arc::new(DecisionFinalizer),
        };
        let _ = runtime.drive(&claim).await;
        let op = store.operations(&run).await.unwrap().remove(0);
        let account = store.account(&account).await.unwrap();
        if payer == OpenRouterPayer::Byok && binding == "deepinfra-standard" {
            assert!(
                matches!(op.result, Some(ExternalResult::Failed { code, .. }) if code == "OPENROUTER_CREDENTIAL_PAYER_MISMATCH")
            );
            assert_eq!(account.reserved, Amount::default());
            http.abort();
        } else {
            let requests = http.await.unwrap();
            assert_eq!(requests.len(), 1);
            let body: Value =
                serde_json::from_str(requests[0].split_once("\r\n\r\n").unwrap().1).unwrap();
            let tags = if multi_route {
                vec!["cloudflare/fp8", "deepinfra/fp8"]
            } else {
                vec!["deepinfra/fp8"]
            };
            assert_eq!(
                body["provider"],
                json!({"only":tags,"allow_fallbacks":false,"require_parameters":true})
            );
            if multi_route {
                assert_eq!(op.request.reserve, Amount::new(32_128, 4_901));
            }
            assert_eq!(op.settled, expected.is_some());
            assert_eq!(
                account.used,
                expected.map_or(Amount::default(), |cost| Amount::new(1100, cost))
            );
        }
    }
}

#[cfg(feature = "openai")]
#[tokio::test]
async fn managed_background_reserves_before_submit_and_settles_only_once() {
    let store = Arc::new(runtrell::memory::MemoryStore::default());
    managed_background_contract(store.clone(), std::future::ready(store)).await;
}

#[cfg(feature = "openai")]
#[tokio::test]
async fn managed_background_resumes_on_an_independent_postgres_pool() {
    use runtrell_postgres::PgStore;
    use sqlx::postgres::PgPoolOptions;
    let url = std::env::var("RUNTRELL_TEST_DATABASE_URL").expect("dedicated test DB");
    let tick = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    let schema = format!("background_{}", tick.as_nanos());
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();
    let first = Arc::new(
        PgStore::initialize(pool, &schema, Capacity::default())
            .await
            .unwrap(),
    );
    managed_background_contract(first, async {
        let other_pool = PgPoolOptions::new().connect(&url).await.unwrap();
        Arc::new(
            PgStore::connect(other_pool, &schema, Capacity::default())
                .await
                .unwrap(),
        )
    })
    .await;
}

#[cfg(feature = "openai")]
async fn managed_background_contract<S: ControlStore>(
    store: Arc<S>,
    reopen: impl std::future::Future<Output = Arc<S>>,
) {
    use runtrell_testkit::{
        BusinessTool, DecisionFinalizer, DecisionValidator, FakePolicy, TestClock,
    };

    let pending = |status| {
        json!({"id":"resp_fixture","object":"response","background":true,
        "model":"gpt-6.1-terra","status":status,"service_tier":"default"})
    };
    let mut terminal = pending("completed");
    terminal["output"] = json!([{"type":"message","role":"assistant","status":"completed",
        "content":[{"type":"output_text","text":"{\"decision\":\"answered\"}"}]}]);
    terminal["usage"] = json!({"input_tokens":100,"input_tokens_details":{"cached_tokens":0,
        "cache_write_tokens":0},"output_tokens":32,"total_tokens":132});
    let (url, http) = sequence_server(vec![
        (
            200,
            "",
            json!({"object":"response.input_tokens","input_tokens":100}),
        ),
        (200, "", pending("queued")),
        (401, "", json!({"error":{"code":"invalid_key"}})),
        (200, "", pending("in_progress")),
        (200, "", terminal),
    ])
    .await;

    let account = store
        .create_account("tenant", Amount::new(10_000_000, 100_000_000), None)
        .await
        .unwrap();
    let run = store
        .start(RunSpec::new(
            "tenant",
            "background",
            account.clone(),
            "answer",
        ))
        .await
        .unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let claim = store.claim(&run, "worker", now, 100_000).await.unwrap();
    let clock = TestClock::default();
    clock.set(now);
    let mut model = connect_model(
        "openai/gpt-6.1-terra",
        Some("standard-background"),
        Connection {
            run: run.clone(),
            session: None,
            credentials: Arc::new(|_| Ok(ApiKey::new("FIXTURE-KEY"))),
            attachment_resolver: None,
            retry_policy: RetryPolicy::default(),
            timeout_ms: 10_000,
            context_cap: None,
            input_token_cap: None,
            max_output_tokens: Some(64),
            reasoning: None,
        },
    )
    .unwrap();
    model.0.config.endpoint = url;
    let runtime = Runtime {
        store: store.clone(),
        model,
        tools: Vec::<BusinessTool>::new(),
        policy: FakePolicy::default(),
        clock,
        validator: Arc::new(DecisionValidator::default()),
        finalizer: Arc::new(DecisionFinalizer),
    };
    runtime.drive(&claim).await.unwrap();
    let operation = store.list_operations(&run).await.unwrap().remove(0);
    assert!(
        matches!(operation.result, Some(ExternalResult::Accepted { ref external_id }) if external_id == "resp_fixture")
    );
    assert_eq!(
        store.account(&account).await.unwrap().reserved,
        operation.request.reserve
    );
    assert!(operation.request.reserve.tokens > 64);
    assert_eq!(operation.send_count, 1);
    let store = reopen.await;
    let runtime = Runtime {
        store: store.clone(),
        ..runtime
    };
    assert!(matches!(
        runtime.drive(&claim).await.unwrap().state,
        RunState::Waiting(_)
    ));
    let after_lookup = store.list_operations(&run).await.unwrap().remove(0);
    assert_eq!(after_lookup.result, operation.result);
    assert_eq!(
        after_lookup.lookup_failure,
        Some(LookupFailure::Rejected(401))
    );
    assert_eq!(after_lookup.usage_outcome(), UsageOutcome::Pending);
    assert!(!after_lookup.unknown_usage);
    for _ in 0..3 {
        if runtime.drive(&claim).await.unwrap().state == RunState::Terminal(Terminal::Succeeded) {
            break;
        }
    }
    let operation = store.list_operations(&run).await.unwrap().remove(0);
    assert!(operation.settled);
    assert_eq!(operation.send_count, 1);
    let receipt = operation.result.unwrap();
    let ExternalResult::Applied { ref usage, .. } = receipt else {
        panic!("terminal provider response")
    };
    let amount = usage.amount().unwrap();
    assert!(amount.micro_usd > 0);
    assert_eq!(amount.tokens, 132);
    store
        .record(&claim, &operation.id, receipt, now)
        .await
        .unwrap();
    let totals = store.account(&account).await.unwrap();
    assert_eq!(totals.used, amount);
    assert_eq!(totals.reserved, Amount::default());

    let requests = http.await.unwrap();
    assert!((3..=5).contains(&requests.len()));
    assert!(requests[0].starts_with("POST /test/input_tokens "));
    assert!(requests[1].starts_with("POST /test "));
    let sent: Value = serde_json::from_str(requests[1].split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(sent["background"], true);
    assert_eq!(sent["max_output_tokens"], 64);
    assert!(
        requests[2..]
            .iter()
            .all(|req| req.starts_with("GET /test/resp_fixture "))
    );
}

#[cfg(feature = "openai")]
#[test]
fn managed_responses_verifies_model_cache_ttl_and_price() {
    let context = context();
    for (name, cost_30m, cost_12h) in [
        ("gpt-6.2-sol", 667, 692),
        ("gpt-6.1-sol", 667, 692),
        ("gpt-6.1-terra", 334, 346),
        ("gpt-6-luna", 34, 35),
    ] {
        for (binding, ttl, expected) in [
            ("standard", "30m", cost_30m),
            ("standard-cache-12h", "12h", cost_12h),
        ] {
            let model = connect_model(
                &format!("openai/{name}"),
                Some(binding),
                Connection {
                    run: context.claim.run.clone(),
                    session: None,
                    credentials: Arc::new(|_| panic!("fixture resolved credential")),
                    attachment_resolver: None,
                    retry_policy: RetryPolicy::default(),
                    timeout_ms: 10_000,
                    context_cap: None,
                    input_token_cap: None,
                    max_output_tokens: Some(64),
                    reasoning: None,
                },
            )
            .unwrap();
            let mut request = request();
            request.model = name.into();
            request.max_output = 64;
            let body = model.0.body(&context.claim.run, &text(request)).unwrap();
            assert_eq!(body["model"], name);
            assert_eq!(body["max_output_tokens"], 64);
            assert_eq!(body["prompt_cache_options"]["ttl"], ttl);
            let mut receipt = json!({"model": name,"service_tier":"default","usage":{"input_tokens":100,
                "input_tokens_details":{"cached_tokens":20,"cache_write_tokens":10},
                "output_tokens":50,"total_tokens":150}});
            assert_eq!(
                model.0.usage(&receipt).unwrap().amount(),
                Ok(Amount::new(150, expected))
            );
            receipt["usage"]["input_tokens_details"]["cache_write_tokens_details"]
                [format!("ttl_{}_tokens", if ttl == "30m" { "12h" } else { "30m" })] = json!(10);
            assert_eq!(
                model.0.usage(&receipt).unwrap().amount(),
                Err(Error::UsageUnknown)
            );
            receipt["usage"]["input_tokens_details"]["cache_write_tokens_details"] = Value::Null;
            for sku in [Value::Null, json!("other-model")] {
                receipt["model"] = sku;
                assert_eq!(
                    model.0.usage(&receipt).unwrap().amount(),
                    Err(Error::UsageUnknown)
                );
            }
        }
    }
}

#[cfg(feature = "gemini")]
#[tokio::test]
async fn managed_gemini_wire_binds_the_thinking_budget_and_verifies_the_sku() {
    use runtrell::memory::MemoryStore;
    use runtrell_testkit::{
        BusinessTool, DecisionFinalizer, DecisionValidator, FakePolicy, TestClock,
    };

    let wire_id = "gemini-2.5-flash-lite";
    let receipt = json!({"modelVersion":wire_id,
        "candidates":[{"content":{"role":"model","parts":[{"text":"{\"decision\":\"answered\"}"}]},"finishReason":"STOP"}],
        "usageMetadata":{"promptTokenCount":100,"cachedContentTokenCount":20,
            "candidatesTokenCount":30,"totalTokenCount":130}});
    let (url, http) = server(200, "", receipt.clone()).await;
    let store = Arc::new(MemoryStore::default());
    let account = store
        .create_account("tenant", Amount::new(2_000_000, 1_000_000), None)
        .await
        .unwrap();
    let run = store
        .start(RunSpec::new("tenant", "gemini", account.clone(), "answer"))
        .await
        .unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let claim = store.claim(&run, "worker", now, 100_000).await.unwrap();
    let clock = TestClock::default();
    clock.set(now);
    let mut model = connect_model(
        "gemini/gemini-2.5-flash-lite",
        None,
        Connection {
            run: run.clone(),
            session: None,
            credentials: Arc::new(|_| Ok(ApiKey::new("FIXTURE-KEY"))),
            attachment_resolver: None,
            retry_policy: RetryPolicy::default(),
            timeout_ms: 10_000,
            context_cap: None,
            input_token_cap: None,
            max_output_tokens: Some(64),
            reasoning: None,
        },
    )
    .unwrap();
    for (field, value) in [
        ("/modelVersion", json!("different-sku")),
        ("/modelVersion", Value::Null),
    ] {
        let mut invalid = receipt.clone();
        *invalid.pointer_mut(field).unwrap() = value;
        assert_eq!(
            model.0.usage(&invalid).unwrap().amount(),
            Err(Error::UsageUnknown)
        );
    }
    let mut thinking = receipt;
    thinking["usageMetadata"]["thoughtsTokenCount"] = json!(1);
    thinking["usageMetadata"]["totalTokenCount"] = json!(131);
    assert_eq!(
        model.0.usage(&thinking).unwrap().amount(),
        Err(Error::UsageUnknown)
    );

    model.0.config.endpoint = url;
    let runtime = Runtime {
        store: store.clone(),
        model,
        tools: Vec::<BusinessTool>::new(),
        policy: FakePolicy::default(),
        clock,
        validator: Arc::new(DecisionValidator::default()),
        finalizer: Arc::new(DecisionFinalizer),
    };
    assert_eq!(
        runtime.drive(&claim).await.unwrap().state,
        RunState::Terminal(Terminal::Succeeded)
    );
    assert_eq!(
        store.account(&account).await.unwrap().used,
        Amount::new(130, 21)
    );
    let requests = http.await.unwrap();
    assert_eq!(requests.len(), 1);
    let sent: Value = serde_json::from_str(requests[0].split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(sent["generationConfig"]["maxOutputTokens"], 64);
    assert_eq!(
        sent["generationConfig"]["thinkingConfig"]["thinkingBudget"],
        0
    );
    assert_eq!(sent["generationConfig"]["candidateCount"], 1);
}

#[cfg(feature = "openai")]
#[test]
fn responses_bound_tracks_prepared_history_and_stops_at_the_input_cap() {
    let mut request = request();
    request.model = "gpt-6.2-sol".into();
    request.max_output = 64;
    let model = connect_model(
        "openai/gpt-6.2-sol",
        None,
        Connection {
            run: context().claim.run,
            session: None,
            credentials: Arc::new(|_| panic!("estimating must not resolve credentials")),
            attachment_resolver: None,
            retry_policy: RetryPolicy::default(),
            timeout_ms: 10_000,
            context_cap: None,
            input_token_cap: Some(32_000),
            max_output_tokens: Some(64),
            reasoning: None,
        },
    )
    .unwrap();
    let bound = |r: &ModelRequest| model.input_upper_bound(&text(r.clone())).unwrap();
    let initial = bound(&request);
    request
        .messages
        .push(Message::text("assistant", "Recent findings. ".repeat(700)));
    let grown = bound(&request);
    assert!(initial < grown && grown < 32_000, "{initial} -> {grown}");
    request
        .messages
        .push(Message::text("user", "long history ".repeat(3000)));
    assert_eq!(bound(&request), 32_000);
}

#[cfg(feature = "openai")]
#[tokio::test]
async fn responses_compacts_grown_file_history_then_writes_the_deliverable() {
    use runtrell::{
        data::{Access, DataStore, Namespace, StoragePolicy, WorkspaceStore},
        file_tools::{FileAction, FileTool},
        memory::MemoryStore,
    };
    use runtrell_testkit::{DecisionFinalizer, DecisionValidator, FakePolicy, TestClock};

    let counted = |tokens| json!({"object":"response.input_tokens","input_tokens":tokens});
    let answer = |id: &str, output, input: u64| {
        json!({"id":id,"model":"gpt-6.2-sol","status":"completed","service_tier":"default",
            "output":[output],"usage":{"input_tokens":input,
            "input_tokens_details":{"cached_tokens":0,"cache_write_tokens":0},
            "output_tokens":32,"total_tokens":input+32}})
    };
    let function = |tool: &str, id: &str, arguments: Value| {
        json!({"type":"function_call","status":"completed","call_id":id,
            "name":digest(&json!(tool)), "arguments":arguments.to_string()})
    };
    let read = |path, revision| json!({"path":path,"revision":revision,"offset":0,"length":4096});
    let message = |text: &str| {
        json!({"type":"message","role":"assistant","status":"completed",
            "content":[{"type":"output_text","text":text}]})
    };
    let summary = "Read input/a.txt and input/b.txt. First found 17, second found 42. Next write work/result.txt with both findings.";
    let (url, http) = raw_server_with_replies(
        vec![
            counted(100),
            answer(
                "resp_1",
                function("files.read", "call_a", read("input/a.txt", 1)),
                100,
            ),
            counted(1200),
            answer(
                "resp_2",
                function("files.read", "call_b", read("input/b.txt", 2)),
                1200,
            ),
            counted(1900),
            answer("resp_summary", message(summary), 1900),
            counted(1250),
            answer(
                "resp_3",
                function(
                    "files.write",
                    "call_save",
                    json!({"path":"work/result.txt","expected_revision":null,"text":"17 and 42"}),
                ),
                1250,
            ),
            counted(1375),
            answer("resp_4", message("{\"decision\":\"saved\"}"), 1375),
        ],
        10,
    )
    .await;

    let store = Arc::new(MemoryStore::default());
    let account = store
        .create_account("tenant", Amount::new(100_000, 1_000_000), None)
        .await
        .unwrap();
    let mut spec = RunSpec::new(
        "tenant",
        "file-progress",
        account.clone(),
        "Read both sources and save the findings in work/result.txt.",
    );
    // The configured input cap binds earlier than the model context window.
    // 20,000 input cap - 13,500 slack = 6,500 input before compaction.
    spec.compaction = Some(CompactionPolicy {
        min_remaining_tokens: 13_500,
        preserve_progress: false,
        max_without_progress: None,
    });
    let run = store.start(spec).await.unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let claim = store.claim(&run, "worker", now, 100_000).await.unwrap();
    let clock = TestClock::default();
    clock.set(now);
    let mut model = connect_model(
        "openai/gpt-6.2-sol",
        None,
        Connection {
            run: run.clone(),
            session: None,
            credentials: Arc::new(|_| Ok(ApiKey::new("FIXTURE-KEY"))),
            attachment_resolver: None,
            retry_policy: RetryPolicy::default(),
            timeout_ms: 10_000,
            context_cap: None,
            input_token_cap: Some(20_000),
            max_output_tokens: Some(64),
            reasoning: None,
        },
    )
    .unwrap();
    model.0.config.endpoint = url;
    let access = Access::owner(Namespace {
        owner: run.clone(),
        agent: "files".into(),
    });
    let data = DataStore::memory(StoragePolicy::default());
    for (path, bytes) in [
        ("input/a.txt", b"First found 17. ".repeat(180)),
        ("input/b.txt", b"Second found 42. ".repeat(105)),
    ] {
        data.write(&access, path, None, &bytes).await.unwrap();
    }
    let tools = [FileAction::Read, FileAction::Write]
        .map(|action| FileTool {
            store: data.clone(),
            access: access.clone(),
            action,
        })
        .to_vec();
    let runtime = Runtime {
        store: store.clone(),
        model,
        tools,
        policy: FakePolicy::default(),
        clock,
        validator: Arc::new(DecisionValidator::default()),
        finalizer: Arc::new(DecisionFinalizer),
    };
    let result = runtime.drive(&claim).await.unwrap();
    let requests = http.await.unwrap();
    let turns = store
        .list_operations(&run)
        .await
        .unwrap()
        .into_iter()
        .filter(|op| {
            matches!(
                op.request.kind,
                OperationKind::Model | OperationKind::Compaction
            )
        })
        .map(|op| {
            (
                op.request.key,
                op.request.reserve.tokens - op.request.input["max_output"].as_u64().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        result.state,
        RunState::Terminal(Terminal::Succeeded),
        "reserved input by turn: {turns:?}"
    );
    let (pairs, extra) = requests.as_chunks::<2>();
    assert!(extra.is_empty());
    let prompts: Vec<Value> = pairs
        .iter()
        .map(|pair| {
            assert!(pair[0].starts_with("POST /test/input_tokens "));
            assert!(pair[1].starts_with("POST /test "));
            let body = |request: &str| {
                serde_json::from_str::<Value>(request.split_once("\r\n\r\n").unwrap().1).unwrap()
            };
            let counted = body(&pair[0]);
            let sent = body(&pair[1]);
            assert_eq!(counted["input"], sent["input"]);
            counted
        })
        .collect();
    let [initial, after_one_read, compact, resumed, after_write] = prompts.as_slice() else {
        panic!("expected four normal turns and one compaction");
    };
    assert!(!initial["input"].to_string().contains("input/a.txt"));
    assert!(
        after_one_read["input"]
            .to_string()
            .contains("First found 17")
    );
    let compact_input = compact["input"].to_string();
    for fact in [
        "current goal",
        "already read or written",
        "input/a.txt",
        "input/b.txt",
        "work/result.txt",
        "First found 17",
        "Second found 42",
    ] {
        assert!(compact_input.contains(fact), "{fact}");
    }
    assert!(resumed["input"].to_string().contains(summary));
    assert!(
        !resumed["input"]
            .to_string()
            .contains(&"First found 17. ".repeat(10))
    );
    assert!(after_write["input"].to_string().contains("work/result.txt"));
    assert_eq!(
        data.read(&access, "work/result.txt", 3, 0, 20)
            .await
            .unwrap()
            .bytes,
        b"17 and 42"
    );

    let operations = store.list_operations(&run).await.unwrap();
    let mut compactions = 0;
    let mut used = Amount::default();
    for op in operations.iter().filter(|op| {
        matches!(
            op.request.kind,
            OperationKind::Model | OperationKind::Compaction
        )
    }) {
        compactions += u64::from(op.request.kind == OperationKind::Compaction);
        assert!(op.settled && !op.unknown_usage && op.send_count == 1);
        let Some(ExternalResult::Applied { usage, .. }) = &op.result else {
            panic!()
        };
        let actual = usage.amount().unwrap();
        assert!(
            actual.tokens <= op.request.reserve.tokens
                && actual.micro_usd <= op.request.reserve.micro_usd
        );
        used.tokens += actual.tokens;
        used.micro_usd += actual.micro_usd;
    }
    assert_eq!(compactions, 1);
    let balance = store.account(&account).await.unwrap();
    assert_eq!(
        (balance.used, balance.reserved, balance.unknown),
        (used, Amount::default(), 0)
    );
}

// SPDX-License-Identifier: Apache-2.0
#![cfg(any(feature = "openai", feature = "gemini", feature = "openrouter"))]
use runtrell::*;
use runtrell_providers::*;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

async fn server(
    status: u16,
    headers: &str,
    body: Value,
) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let body = body.to_string();
    raw_server(format!(
        "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}", body.len()
    )).await
}
async fn raw_server(response: String) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/test", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        while let Ok(Ok((mut stream, _))) =
            tokio::time::timeout(Duration::from_millis(250), listener.accept()).await
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
                        .unwrap()
                        .parse()
                        .unwrap();
                    if bytes.len() >= end + 4 + len {
                        break;
                    }
                }
            }
            requests.push(String::from_utf8(bytes).unwrap());
            let _ = stream.write_all(response.as_bytes()).await;
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
                timeout: 2,
            },
            json_schema: true,
            tool_calling: true,
            parallel_tool_calls: true,
            image_input: true,
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
        run: RunHandle {
            tenant: "tenant".into(),
            id: "run-a".into(),
        },
        session: Some("session-a".into()),
        input_token_upper_bound: 100,
        max_request_bytes: 65536,
        max_response_bytes: 65536,
    }
}
fn context() -> ToolContext {
    ToolContext {
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
        model: "host-model".into(),
        messages: vec![Message::text("user", "hello")],
        tools: vec![],
        max_output: 128,
        metadata: Value::Null,
    }
}
async fn wire_contract<T: ModelPort + std::fmt::Debug>(
    make: fn(Config) -> Result<T>,
    kind: &str,
    response: Value,
) {
    let (url, task) = server(200, "", response).await;
    let adapter = make(config(url)).unwrap();
    assert!(!format!("{adapter:?}").contains("TEST-KEY-DO-NOT-PRINT"));
    let result = adapter.invoke(&context(), request()).await.unwrap();
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
        let (reply, usage) = applied(model.invoke(&context(), tool_request()).await.unwrap());
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
    let (reply, _) = applied(model.invoke(&context(), tool_request()).await.unwrap());
    assert_eq!(reply.metadata["stop_reason"], "max_tokens");
    assert!(reply.calls.is_empty());
    assert_eq!(task.await.unwrap().len(), 1);
}
async fn errors<T: ModelPort + std::fmt::Debug>(make: fn(Config) -> Result<T>) {
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
        let result = model.invoke(&context(), request()).await.unwrap();
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
        assert_eq!(retry_after, Some(7));
        assert_eq!(usage.input, None);
        assert_eq!(usage.output, None);
        assert_eq!(task.await.unwrap().len(), 1, "hidden retry");
    }
    let date = httpdate::fmt_http_date(std::time::SystemTime::now() + Duration::from_secs(60));
    let (url, task) = server(429, &format!("Retry-After: {date}\r\n"), json!({})).await;
    let model = make(config(url)).unwrap();
    let ExternalResult::Failed {
        retry_after: Some(delay),
        ..
    } = model.invoke(&context(), request()).await.unwrap()
    else {
        panic!("date delay")
    };
    assert!((58..=60).contains(&delay));
    assert_eq!(task.await.unwrap().len(), 1);
    let (target, target_task) = server(200, "", json!({})).await;
    let (url, task) = server(307, &format!("Location: {target}\r\n"), json!({})).await;
    let model = make(config(url)).unwrap();
    let ExternalResult::Failed { code, .. } = model.invoke(&context(), request()).await.unwrap()
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
    for raw in [
        String::new(),
        "HTTP/1.1 200 OK\r\nContent-Length: 99\r\nConnection: close\r\n\r\n{".into(),
        "HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nnope".into(),
    ] {
        let (url, task) = raw_server(raw).await;
        let model = make(config(url)).unwrap();
        assert_eq!(
            model.invoke(&context(), request()).await.unwrap(),
            ExternalResult::Unknown
        );
        assert_eq!(task.await.unwrap().len(), 1, "disconnect retry");
    }
}
async fn continuation<T: ModelPort>(make: fn(Config) -> Result<T>, kind: &str) {
    let (url, task) = server(200, "", response(kind, "tool_call", true)).await;
    let model = make(config(url.clone())).unwrap();
    let (reply, _) = applied(model.invoke(&context(), tool_request()).await.unwrap());
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
    restored.invoke(&context(), next.clone()).await.unwrap();
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
            restored.invoke(&context(), forged).await.is_err(),
            "{field}"
        );
    }
    let mut other_context = context();
    other_context.claim.run.id = "other".into();
    assert_eq!(
        model
            .invoke(&other_context, next.clone())
            .await
            .unwrap_err(),
        Error::PermissionDenied
    );
    let mut other_config = config(url.clone());
    other_config.session = Some("other".into());
    let other = make(other_config).unwrap();
    assert_eq!(
        other.invoke(&context(), next.clone()).await.unwrap_err(),
        Error::PermissionDenied
    );
    let mut other_config = config(url);
    other_config.run = other_context.claim.run.clone();
    let other = make(other_config).unwrap();
    assert_eq!(
        other
            .invoke(&other_context, next.clone())
            .await
            .unwrap_err(),
        Error::PermissionDenied
    );
    assert_eq!(
        model.lookup(&context(), next).await.unwrap(),
        Lookup::Unknown
    );
    assert_eq!(model.recovery(), None);
    assert_eq!(model.retry_policy().max_attempts, 1);
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
    assert_eq!(model.input_upper_bound(&request()).unwrap(), 100);
    let mut req = tool_request();
    req.messages
        .insert(0, Message::text("system", "be helpful"));
    req.metadata = json!({"previous_response_id":"untrusted-metadata-never-forwarded","provider_options":{"json_schema":{"type":"object"},"images":[{"mime_type":"image/png","data_base64":"aGVsbG8="}]}});
    model.invoke(&context(), req.clone()).await.unwrap();
    for max in [0, 9, 1025] {
        let mut bad = request();
        bad.max_output = max;
        assert!(model.invoke(&context(), bad).await.is_err());
    }
    let mut bad = request();
    bad.model = "wrong".into();
    assert!(model.invoke(&context(), bad).await.is_err());
    let mut bad = request();
    bad.metadata = json!({"provider_options":{"previous_response_id":"raw-id"}});
    assert!(model.invoke(&context(), bad).await.is_err());
    let mut ctx = context();
    ctx.deadline = Some(0);
    assert_eq!(
        model.invoke(&ctx, request()).await.unwrap_err(),
        Error::Deadline
    );
    cfg.capabilities.json_schema = false;
    assert!(
        make(cfg.clone())
            .unwrap()
            .invoke(&context(), req.clone())
            .await
            .is_err()
    );
    cfg.capabilities.json_schema = true;
    cfg.capabilities.image_input = false;
    assert!(
        make(cfg.clone())
            .unwrap()
            .invoke(&context(), req.clone())
            .await
            .is_err()
    );
    cfg.capabilities.image_input = true;
    cfg.capabilities.tool_calling = false;
    cfg.capabilities.parallel_tool_calls = false;
    assert!(
        make(cfg.clone())
            .unwrap()
            .invoke(&context(), tool_request())
            .await
            .is_err()
    );
    cfg.capabilities.tool_calling = true;
    cfg.max_request_bytes = 1;
    assert!(
        make(cfg.clone())
            .unwrap()
            .invoke(&context(), request())
            .await
            .is_err()
    );
    cfg.max_request_bytes = 65536;
    cfg.max_response_bytes = 1;
    assert_eq!(
        make(cfg.clone())
            .unwrap()
            .invoke(&context(), request())
            .await
            .unwrap(),
        ExternalResult::Unknown
    );
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
            assert_eq!(body["input"][2]["content"][0]["type"], "input_image");
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
            assert_eq!(
                body["contents"][1]["parts"][0]["inlineData"]["mimeType"],
                "image/png"
            );
        }
        _ => {
            assert_eq!(body["response_format"]["type"], "json_schema");
            assert_eq!(body["tools"][0]["function"]["name"], "lookup");
            assert_eq!(body["messages"][2]["content"][0]["type"], "image_url");
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
                .invoke(&context(), request())
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
            .invoke(&context(), request())
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
                errors($adapter::new).await;
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
                .invoke(&context(), request())
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
        .invoke(&context(), request())
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

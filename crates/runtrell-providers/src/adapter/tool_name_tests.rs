// SPDX-License-Identifier: Apache-2.0
use super::*;

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
fn adapter(wire: Wire) -> Adapter {
    Adapter::new(config("http://127.0.0.1/test".into()), wire).unwrap()
}
fn prepared(names: &[&str]) -> PreparedModelRequest {
    let mut req = tool_request();
    let tool = req.tools[0].clone();
    req.tools = names
        .iter()
        .map(|name| {
            let mut t = tool.clone();
            t.name = (*name).into();
            t
        })
        .collect();
    for role in ["assistant", "tool"] {
        req.messages.push(Message {
            role: role.into(),
            text: String::new(),
            call_id: Some("old-call".into()),
            tool: Some(names[0].into()),
            data: json!({"q":1}),
        });
    }
    PreparedModelRequest::text(req).unwrap()
}
fn definition_names(body: &Value, wire: Wire) -> Vec<String> {
    let definitions = if wire == Wire::GenerateContent {
        &body["tools"][0]["functionDeclarations"]
    } else {
        &body["tools"]
    };
    definitions
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (if wire == Wire::Chat {
                &t["function"]["name"]
            } else {
                &t["name"]
            })
            .as_str()
            .unwrap()
            .to_owned()
        })
        .collect()
}
#[test]
fn definitions_history_and_restore() {
    for wire in [Wire::Responses, Wire::Chat, Wire::GenerateContent] {
        let a = adapter(wire);
        let long = "x".repeat(65);
        let boundary = "x".repeat(64);
        let names = [
            "files.list",
            "files_list",
            long.as_str(),
            "ok-name",
            "x",
            boundary.as_str(),
            "tools/検索",
        ];
        let req = prepared(&names);
        let body = a.body(&a.config.run, &req).unwrap();
        let actual = definition_names(&body, wire);
        let expected = if wire == Wire::GenerateContent {
            "files.list".into()
        } else {
            digest(&json!("files.list"))
        };
        assert_eq!(actual[0], expected);
        assert_eq!(actual[1], "files_list");
        assert_eq!(actual[5], boundary);
        if wire == Wire::GenerateContent {
            assert_eq!(actual, names);
        }
        if wire != Wire::GenerateContent {
            for n in &actual {
                assert!(
                    n.len() <= 64
                        && n.bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                );
            }
        }
        match wire {
            Wire::Responses => assert_eq!(body["input"][1]["name"], expected),
            Wire::Chat => assert_eq!(
                body["messages"][1]["tool_calls"][0]["function"]["name"],
                expected
            ),
            Wire::GenerateContent => assert_eq!(
                body["contents"][1]["parts"][0]["functionCall"]["name"],
                expected
            ),
        }
        let kind = match wire {
            Wire::Responses => "openai",
            Wire::Chat => "chat",
            Wire::GenerateContent => "gemini",
        };
        let mut v = response(kind, "tool_call", true);
        let path = match wire {
            Wire::Responses => "/output/0/name",
            Wire::Chat => "/choices/0/message/tool_calls/0/function/name",
            Wire::GenerateContent => "/candidates/0/content/parts/0/functionCall/name",
        };
        *v.pointer_mut(path).unwrap() = json!(expected);
        let ExternalResult::Applied { output, .. } = a.reply(&v, &req).unwrap() else {
            panic!()
        };
        let reply: ModelReply = serde_json::from_value(output).unwrap();
        assert_eq!(reply.calls[0].name, "files.list");
        assert_eq!(reply.calls[0].id, "call-1");
        let native = match wire {
            Wire::Responses => &v["output"],
            Wire::Chat => &v["choices"][0]["message"],
            Wire::GenerateContent => &v["candidates"][0]["content"],
        };
        assert_eq!(&reply.metadata["continuation"]["content"], native);
        let mut resumed = (*req).clone();
        resumed.tools.reverse();
        resumed.messages.push(Message {
            role: "provider".into(),
            text: String::new(),
            call_id: None,
            tool: None,
            data: reply.metadata,
        });
        let restored = adapter(wire);
        let replay = restored
            .body(
                &restored.config.run,
                &PreparedModelRequest::text(resumed).unwrap(),
            )
            .unwrap();
        let mut reversed = definition_names(&replay, wire);
        reversed.reverse();
        assert_eq!(reversed, actual);
        match wire {
            Wire::Responses => assert_eq!(replay["input"][3]["name"], expected),
            Wire::Chat => assert_eq!(
                replay["messages"][3]["tool_calls"][0]["function"]["name"],
                expected
            ),
            Wire::GenerateContent => assert_eq!(
                replay["contents"][3]["parts"][0]["functionCall"]["name"],
                expected
            ),
        }
        *v.pointer_mut(path).unwrap() = json!("unknown_wire");
        let ExternalResult::Applied { output, .. } = a.reply(&v, &req).unwrap() else {
            panic!()
        };
        assert_eq!(output["metadata"]["stop_reason"], "unknown");
        assert_eq!(output["calls"], json!([]));
    }
}

#[test]
fn historical_tools_need_not_be_in_current_catalog() {
    for wire in [Wire::Responses, Wire::Chat] {
        let a = adapter(wire);
        let req = prepared(&["files.list", "files_list"]);
        let mut logical = (*req).clone();
        logical.tools.remove(0);
        let req = PreparedModelRequest::text(logical).unwrap();
        let identity = digest(&serde_json::to_value(&*req).unwrap());
        let body = a.body(&a.config.run, &req).unwrap();
        assert_eq!(definition_names(&body, wire), ["files_list"]);
        let alias = digest(&json!("files.list"));
        match wire {
            Wire::Responses => assert_eq!(body["input"][1]["name"], alias),
            Wire::Chat => assert_eq!(
                body["messages"][1]["tool_calls"][0]["function"]["name"],
                alias
            ),
            _ => unreachable!(),
        }
        assert_eq!(digest(&serde_json::to_value(&*req).unwrap()), identity);
        // Even a known historical alias cannot invoke a removed tool.
        let mut v = response(
            if wire == Wire::Responses {
                "openai"
            } else {
                "chat"
            },
            "tool_call",
            true,
        );
        let path = if wire == Wire::Responses {
            "/output/0/name"
        } else {
            "/choices/0/message/tool_calls/0/function/name"
        };
        for returned in [alias.as_str(), "files.list"] {
            *v.pointer_mut(path).unwrap() = json!(returned);
            let ExternalResult::Applied { output, .. } = a.reply(&v, &req).unwrap() else {
                panic!()
            };
            assert_eq!(output["metadata"]["stop_reason"], "unknown");
            assert_eq!(output["calls"], json!([]));
        }
    }
}
#[test]
fn wire_collision_is_a_preflight_error() {
    let collision = digest(&json!("files.list"));
    let req = prepared(&["files.list", &collision]);
    for wire in [Wire::Responses, Wire::Chat] {
        let a = adapter(wire);
        assert!(a.body(&a.config.run, &req).is_err());
    }
    let a = adapter(Wire::GenerateContent);
    assert!(a.body(&a.config.run, &req).is_ok());
}

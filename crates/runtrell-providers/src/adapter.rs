// SPDX-License-Identifier: Apache-2.0
use crate::*;
use reqwest::{Client, Url, header::HeaderValue};
use runtrell::*;
use serde_json::json;
use std::time::{Duration, SystemTime};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Wire {
    Responses,
    GenerateContent,
    Chat,
}
#[derive(Debug)]
pub(crate) struct Adapter {
    pub(crate) config: Config,
    client: Client,
    pub(crate) wire: Wire,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    version: u32,
    run: RunHandle,
    session: Option<String>,
    endpoint: String,
    model: String,
    wire: Wire,
    response_id: Option<String>,
    content: Value,
}
#[allow(clippy::large_enum_variant)] // Forward the original settlement receipt inline.
pub(crate) enum HttpResult {
    Json(Value),
    Failure(ExternalResult),
    Unknown,
}
fn invalid() -> Error {
    Error::Invalid("provider request or configuration".into())
}
impl Adapter {
    pub(crate) fn new(config: Config, wire: Wire) -> Result<Self> {
        config.image_limits.validate()?;
        let url = Url::parse(&config.endpoint).map_err(|_| invalid())?;
        let l = &config.capabilities.limits;
        if !(url.scheme() == "https"
            || (url.scheme() == "http"
                && matches!(url.host_str(), Some("127.0.0.1" | "[::1]" | "localhost"))))
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || config.key.0.is_empty()
            || config.endpoint.contains(&config.key.0)
            || HeaderValue::from_str(&config.key.0).is_err()
            || l.model.is_empty()
            || l.context == 0
            || l.max_output == 0
            || l.min_max_output_tokens == 0
            || l.min_max_output_tokens > l.max_output
            || l.max_output > l.context
            || l.price.as_ref().is_some_and(|p| p.model != l.model)
            || l.timeout == 0
            || config.max_request_bytes == 0
            || config.max_response_bytes == 0
            || config.input_token_upper_bound == 0
            || config.input_token_upper_bound >= l.context
            || (config.capabilities.parallel_tool_calls && !config.capabilities.tool_calling)
        {
            return Err(invalid());
        }
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .referer(false)
            .timeout(Duration::from_secs(l.timeout))
            .build()
            .map_err(|_| invalid())?;
        Ok(Self {
            config,
            client,
            wire,
        })
    }
    fn state(&self, message: &Message) -> Result<State> {
        let state: State = serde_json::from_value(
            message
                .data
                .get("continuation")
                .cloned()
                .ok_or(Error::PermissionDenied)?,
        )
        .map_err(|_| Error::PermissionDenied)?;
        if state.version != 1
            || state.run != self.config.run
            || state.session != self.config.session
            || state.endpoint != self.config.endpoint
            || state.model != self.config.capabilities.limits.model
            || state.wire != self.wire
        {
            return Err(Error::PermissionDenied);
        }
        Ok(state)
    }
    fn body(&self, run: &RunHandle, request: &PreparedModelRequest) -> Result<Value> {
        if *run != self.config.run {
            return Err(Error::PermissionDenied);
        }
        self.document_bound(request)?;
        let c = &self.config.capabilities;
        let l = &c.limits;
        if request.model != l.model
            || request.max_output < l.min_max_output_tokens
            || request.max_output > l.max_output
            || self
                .config
                .input_token_upper_bound
                .checked_add(request.max_output)
                .is_none_or(|n| n > l.context)
            || (!request.tools.is_empty() && !c.tool_calling)
        {
            return Err(invalid());
        }
        let options: RequestOptions = request
            .metadata
            .get("provider_options")
            .map(|v| serde_json::from_value(v.clone()))
            .transpose()
            .map_err(|_| invalid())?
            .unwrap_or_default();
        if options.json_schema.is_some() && !c.json_schema {
            return Err(invalid());
        }
        if !request.resolved_images().is_empty() && !c.images.input {
            return Err(Error::Unsupported("image_input".into()));
        }
        if request.resolved_images().len() > self.config.image_limits.max_images {
            return Err(Error::Invalid("IMAGE_COUNT_LIMIT".into()));
        }
        let images = request.resolved_images();
        for image in images {
            image.validate(&self.config.image_limits)?;
        }
        let mut messages = Vec::new();
        let mut systems = Vec::new();
        let mut replaying = false;
        for m in &request.messages {
            if m.role == "provider" {
                let state = self.state(m)?;
                match self.wire {
                    Wire::Responses => {
                        messages.extend(state.content.as_array().ok_or_else(invalid)?.clone())
                    }
                    _ if !state.content.is_null() => messages.push(state.content),
                    _ => (),
                }
                replaying = true;
                continue;
            }
            if m.role == "assistant" && replaying {
                continue;
            }
            if m.role != "assistant" {
                replaying = false;
            }
            if !matches!(
                m.role.as_str(),
                "user" | "assistant" | "system" | "developer" | "tool"
            ) {
                return Err(invalid());
            }
            let output = if m.text.is_empty() {
                m.data.to_string()
            } else {
                m.text.clone()
            };
            let content = match self.wire {
                Wire::Responses => {
                    if m.role == "tool" {
                        json!({"type":"function_call_output","call_id":m.call_id.as_ref().ok_or_else(invalid)?,"output":output})
                    } else if let Some(id) = &m.call_id {
                        json!({"type":"function_call","call_id":id,"name":m.tool.as_ref().ok_or_else(invalid)?,"arguments":m.data.to_string()})
                    } else {
                        json!({"role":m.role,"content":[{"type":if m.role == "assistant" {"output_text"} else {"input_text"},"text":m.text}]})
                    }
                }
                Wire::Chat => {
                    if m.role == "tool" {
                        json!({"role":"tool","tool_call_id":m.call_id.as_ref().ok_or_else(invalid)?,"content":output})
                    } else if let Some(id) = &m.call_id {
                        json!({"role":"assistant","tool_calls":[{"id":id,"type":"function","function":{"name":m.tool.as_ref().ok_or_else(invalid)?,"arguments":m.data.to_string()}}]})
                    } else {
                        json!({"role":m.role,"content":m.text})
                    }
                }
                Wire::GenerateContent => {
                    if matches!(m.role.as_str(), "system" | "developer") {
                        systems.push(json!({"text":m.text}));
                        continue;
                    }
                    let part = if m.role == "tool" {
                        json!({"functionResponse":{"id":m.call_id,"name":m.tool.as_ref().ok_or_else(invalid)?,"response":{"result":if m.data.is_null() {json!(m.text)} else {m.data.clone()}}}})
                    } else if m.call_id.is_some() {
                        json!({"functionCall":{"id":m.call_id,"name":m.tool.as_ref().ok_or_else(invalid)?,"args":m.data}})
                    } else {
                        json!({"text":m.text})
                    };
                    json!({"role":if m.role == "assistant" {"model"} else {"user"},"parts":[part]})
                }
            };
            messages.push(content);
        }
        if !images.is_empty() {
            // Images belong to a new user turn, never to a provider-state envelope.
            match self.wire {
                Wire::Responses => messages.push(json!({"role":"user","content":images.iter().map(|i| json!({"type":"input_image","image_url":format!("data:{};base64,{}",i.mime_type(),i.data_base64())})).collect::<Vec<_>>()})),
                Wire::Chat => messages.push(json!({"role":"user","content":images.iter().map(|i| json!({"type":"image_url","image_url":{"url":format!("data:{};base64,{}",i.mime_type(),i.data_base64())}})).collect::<Vec<_>>()})),
                Wire::GenerateContent => messages.push(json!({"role":"user","parts":images.iter().map(|i| json!({"inlineData":{"mimeType":i.mime_type(),"data":i.data_base64()}})).collect::<Vec<_>>()})),
            }
        }
        let functions: Vec<_> = request.tools.iter().map(|t| match self.wire {
            Wire::Responses => json!({"type":"function","name":t.name,"description":t.capability,"parameters":t.input_schema,"strict":false}),
            Wire::Chat => json!({"type":"function","function":{"name":t.name,"description":t.capability,"parameters":t.input_schema}}),
            Wire::GenerateContent => json!({"name":t.name,"description":t.capability,"parametersJsonSchema":t.input_schema}),
        }).collect();
        let mut body = match self.wire {
            Wire::Responses => {
                json!({"model":l.model,"input":messages,"max_output_tokens":request.max_output,"store":false,"include":["reasoning.encrypted_content"]})
            }
            Wire::Chat => {
                json!({"model":l.model,"messages":messages,"max_tokens":request.max_output,"stream":false})
            }
            Wire::GenerateContent => {
                json!({"contents":messages,"generationConfig":{"maxOutputTokens":request.max_output,"candidateCount":1}})
            }
        };
        if !functions.is_empty() {
            body["tools"] = match self.wire {
                Wire::GenerateContent => json!([{"functionDeclarations":functions}]),
                _ => json!(functions),
            };
            if self.wire != Wire::GenerateContent {
                body["parallel_tool_calls"] = json!(c.parallel_tool_calls);
            }
        }
        if !systems.is_empty() {
            body["systemInstruction"] = json!({"parts":systems});
        }
        if let Some(schema) = options.json_schema {
            match self.wire {
                Wire::Responses => {
                    body["text"] = json!({"format":{"type":"json_schema","name":"response","strict":true,"schema":schema}})
                }
                Wire::Chat => {
                    body["response_format"] = json!({"type":"json_schema","json_schema":{"name":"response","strict":true,"schema":schema}})
                }
                Wire::GenerateContent => {
                    body["generationConfig"]["responseMimeType"] = json!("application/json");
                    body["generationConfig"]["responseJsonSchema"] = schema;
                }
            }
        }
        Ok(body)
    }
    async fn invoke(
        &self,
        context: &ToolContext,
        request: PreparedModelRequest,
    ) -> Result<ExternalResult> {
        request.validate_for_send()?;
        let mut body = self.body(&context.claim.run, &request)?;
        self.check_document_wire_bound(&request, &body)?;
        let resolved = match self.resolve_documents(context, &request).await {
            Ok(resolved) => resolved,
            Err(_) => {
                return Ok(ExternalResult::Failed {
                    physical_evidence: None,
                    code: "ATTACHMENT_NOT_SENT".into(),
                    usage: Usage {
                        model: request.model.clone(),
                        ..Usage::measured(0, 0, 0)
                    },
                    retry_after: None,
                });
            }
        };
        self.attach_documents(&mut body, &request, &resolved);
        match self.send(context, body, &resolved).await? {
            HttpResult::Json(parsed) => Ok(self
                .reply(&parsed, &request)
                .unwrap_or(ExternalResult::Unknown)),
            HttpResult::Failure(result) => Ok(result),
            HttpResult::Unknown => Ok(ExternalResult::Unknown),
        }
    }
    pub(crate) async fn send(
        &self,
        context: &ToolContext,
        body: Value,
        sensitive: &[runtrell::attachments::ResolvedAttachment],
    ) -> Result<HttpResult> {
        self.send_at(context, &self.config.endpoint, body, sensitive)
            .await
    }
    pub(crate) async fn send_at(
        &self,
        context: &ToolContext,
        endpoint: &str,
        body: Value,
        sensitive: &[runtrell::attachments::ResolvedAttachment],
    ) -> Result<HttpResult> {
        if context.claim.run != self.config.run {
            return Err(Error::PermissionDenied);
        }
        let bytes = serde_json::to_vec(&body).map_err(|_| invalid())?;
        if bytes.len() > self.config.max_request_bytes {
            return Err(invalid());
        }
        let mut timeout = Duration::from_secs(self.config.capabilities.limits.timeout);
        if let Some(deadline) = context.deadline {
            let now = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_err(|_| Error::Deadline)?
                .as_secs();
            if deadline <= now {
                return Err(Error::Deadline);
            }
            timeout = timeout.min(Duration::from_secs(deadline - now));
        }
        let (header, secret) = if self.wire == Wire::GenerateContent {
            ("x-goog-api-key", self.config.key.0.clone())
        } else {
            ("authorization", format!("Bearer {}", self.config.key.0))
        };
        let mut value = HeaderValue::from_str(&secret).map_err(|_| invalid())?;
        value.set_sensitive(true);
        let response = self
            .client
            .post(endpoint)
            .timeout(timeout)
            .header(header, value)
            .header("content-type", "application/json")
            .body(bytes)
            .send()
            .await;
        let Ok(mut response) = response else {
            return Ok(HttpResult::Unknown);
        };
        let status = response.status().as_u16();
        let retry_header = response
            .headers()
            .get("retry-after")
            .and_then(|h| h.to_str().ok())
            .and_then(safe_retry_after);
        let mut bytes = Vec::new();
        loop {
            match response.chunk().await {
                Ok(Some(chunk))
                    if bytes
                        .len()
                        .checked_add(chunk.len())
                        .is_some_and(|n| n <= self.config.max_response_bytes) =>
                {
                    bytes.extend_from_slice(&chunk)
                }
                Ok(None) => break,
                _ => return Ok(HttpResult::Unknown),
            }
        }
        let mut parsed: Value = match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(_) if !(200..300).contains(&status) => Value::Null,
            Err(_) => return Ok(HttpResult::Unknown),
        };
        crate::documents::redact_echoes(&mut parsed, sensitive);
        if !(200..300).contains(&status) || !parsed["error"].is_null() {
            let effective_status = parsed["error"]["code"]
                .as_u64()
                .and_then(|n| u16::try_from(n).ok())
                .filter(|n| (400..600).contains(n))
                .unwrap_or(status);
            let failure = Failure {
                class: classify(effective_status, &parsed),
                status: effective_status,
                retry_after: retry_header.clone(),
            };
            let delay = retry_header.as_deref().and_then(retry_delay);
            return Ok(HttpResult::Failure(ExternalResult::Failed {
                physical_evidence: None,
                code: serde_json::to_string(&failure).map_err(|_| invalid())?,
                usage: self.usage(&parsed).unwrap_or_else(|_| Usage {
                    model: self.config.capabilities.limits.model.clone(),
                    ..Usage::default()
                }),
                retry_after: delay,
            }));
        }
        Ok(HttpResult::Json(parsed))
    }
    fn reply(&self, v: &Value, request: &PreparedModelRequest) -> Result<ExternalResult> {
        let usage = self.usage(v)?;
        let mut text = String::new();
        let mut calls = Vec::new();
        let mut partial_calls = false;
        let (mut stop, mut content, external_id) = match self.wire {
            Wire::Responses => {
                let items = v["output"].as_array().ok_or_else(invalid)?;
                let mut partial = false;
                let mut refused = false;
                for item in items {
                    match item["type"].as_str() {
                        Some("message") => {
                            for part in item["content"].as_array().ok_or_else(invalid)? {
                                if let Some(s) = part["text"].as_str() {
                                    text.push_str(s);
                                }
                                if part["type"] == "refusal" {
                                    refused = true;
                                }
                            }
                        }
                        Some("function_call") => {
                            if item["status"] != "completed" {
                                partial = true;
                                continue;
                            }
                            match parse_call(
                                item["call_id"].as_str(),
                                item["name"].as_str(),
                                item["arguments"]
                                    .as_str()
                                    .and_then(|s| serde_json::from_str(s).ok()),
                            ) {
                                Ok(call) => calls.push(call),
                                Err(_) => partial = true,
                            }
                        }
                        _ => (),
                    }
                }
                let stop = match (
                    v["status"].as_str(),
                    v["incomplete_details"]["reason"].as_str(),
                ) {
                    (_, Some("max_output_tokens")) => StopReason::MaxTokens,
                    (_, Some("content_filter")) => StopReason::ContentFilter,
                    _ if refused => StopReason::ContentFilter,
                    (Some("completed"), _) if !partial => {
                        if calls.is_empty() {
                            StopReason::Completed
                        } else {
                            StopReason::ToolCall
                        }
                    }
                    _ => StopReason::Unknown,
                };
                (stop, json!(items), v["id"].as_str())
            }
            Wire::Chat => {
                let choice = &v["choices"][0];
                let message = &choice["message"];
                if let Some(s) = message["content"].as_str() {
                    text = s.into();
                }
                if let Some(items) = message["tool_calls"].as_array() {
                    for item in items {
                        match parse_call(
                            item["id"].as_str(),
                            item["function"]["name"].as_str(),
                            item["function"]["arguments"]
                                .as_str()
                                .and_then(|s| serde_json::from_str(s).ok()),
                        ) {
                            Ok(call) => calls.push(call),
                            Err(_) => partial_calls = true,
                        }
                    }
                }
                let stop = match choice["finish_reason"].as_str() {
                    Some("stop") if calls.is_empty() => StopReason::Completed,
                    Some("length") => StopReason::MaxTokens,
                    Some("content_filter") => StopReason::ContentFilter,
                    Some("tool_calls") if !calls.is_empty() => StopReason::ToolCall,
                    _ => StopReason::Unknown,
                };
                (stop, message.clone(), v["id"].as_str())
            }
            Wire::GenerateContent => {
                let candidate = &v["candidates"][0];
                let parts = candidate["content"]["parts"].as_array();
                if let Some(parts) = parts {
                    for (index, part) in parts.iter().enumerate() {
                        if part["thought"] != true
                            && let Some(s) = part["text"].as_str()
                        {
                            text.push_str(s);
                        }
                        if let Some(call) = part.get("functionCall") {
                            let id = call["id"]
                                .as_str()
                                .map(str::to_owned)
                                .unwrap_or_else(|| format!("call-{index}"));
                            match parse_call(
                                Some(&id),
                                call["name"].as_str(),
                                call.get("args").cloned(),
                            ) {
                                Ok(call) => calls.push(call),
                                Err(_) => partial_calls = true,
                            }
                        }
                    }
                }
                let stop = if !v["promptFeedback"]["blockReason"].is_null() {
                    StopReason::ContentFilter
                } else {
                    match candidate["finishReason"].as_str() {
                        Some("STOP") => {
                            if calls.is_empty() {
                                StopReason::Completed
                            } else {
                                StopReason::ToolCall
                            }
                        }
                        Some("MAX_TOKENS") => StopReason::MaxTokens,
                        Some(
                            "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII"
                            | "IMAGE_SAFETY",
                        ) => StopReason::ContentFilter,
                        _ => StopReason::Unknown,
                    }
                };
                (stop, candidate["content"].clone(), v["responseId"].as_str())
            }
        };
        if (partial_calls && matches!(stop, StopReason::Completed | StopReason::ToolCall))
            || (stop == StopReason::ToolCall
                && (calls
                    .iter()
                    .any(|c| !request.tools.iter().any(|t| t.name == c.name))
                    || (!self.config.capabilities.parallel_tool_calls && calls.len() > 1)
                    || calls.iter().enumerate().any(|(index, call)| {
                        calls[..index].iter().any(|previous| previous.id == call.id)
                    })))
        {
            stop = StopReason::Unknown;
        }
        if stop != StopReason::ToolCall {
            calls.clear();
            // Never replay an incomplete call as a completed assistant call.
            match self.wire {
                Wire::Responses => {
                    if let Some(items) = content.as_array_mut() {
                        items.retain(|i| i["type"] != "function_call");
                    }
                }
                Wire::Chat => {
                    if let Some(m) = content.as_object_mut() {
                        m.remove("tool_calls");
                    }
                    if text.is_empty() {
                        content = Value::Null;
                    }
                }
                Wire::GenerateContent => {
                    if let Some(parts) = content.get_mut("parts").and_then(Value::as_array_mut) {
                        parts.retain(|p| p.get("functionCall").is_none());
                    }
                    if content["parts"].as_array().is_none_or(Vec::is_empty) {
                        content = Value::Null;
                    }
                }
            }
        }
        let usage_status = if usage.input.is_some() && usage.output.is_some() {
            UsageStatus::TotalsReported
        } else if v
            .get(if self.wire == Wire::GenerateContent {
                "usageMetadata"
            } else {
                "usage"
            })
            .is_none_or(Value::is_null)
        {
            UsageStatus::Absent
        } else {
            UsageStatus::Partial
        };
        let state = State {
            version: 1,
            run: self.config.run.clone(),
            session: self.config.session.clone(),
            endpoint: self.config.endpoint.clone(),
            model: self.config.capabilities.limits.model.clone(),
            wire: self.wire,
            response_id: external_id.map(str::to_owned),
            content,
        };
        let reply = ModelReply {
            text,
            calls,
            complete: !matches!(stop, StopReason::ContentFilter | StopReason::Unknown),
            metadata: json!({"stop_reason":stop,"usage_status":usage_status,"continuation":state}),
        };
        Ok(ExternalResult::Applied {
            physical_evidence: None,
            output: serde_json::to_value(reply).map_err(|_| invalid())?,
            usage,
            external_id: external_id.map(str::to_owned),
        })
    }
    pub(crate) fn usage(&self, v: &Value) -> Result<Usage> {
        let mut u = Usage {
            model: v[if self.wire == Wire::GenerateContent {
                "modelVersion"
            } else {
                "model"
            }]
            .as_str()
            .unwrap_or(&self.config.capabilities.limits.model)
            .into(),
            ..Usage::default()
        };
        let n = |path: &str| -> Result<Option<u64>> {
            match v.pointer(path) {
                None | Some(Value::Null) => Ok(None),
                Some(value) => value.as_u64().map(Some).ok_or_else(invalid),
            }
        };
        match self.wire {
            Wire::Responses => {
                u.input = n("/usage/input_tokens")?;
                u.output = n("/usage/output_tokens")?;
                u.cached = n("/usage/input_tokens_details/cached_tokens")?;
                u.reasoning = n("/usage/output_tokens_details/reasoning_tokens")?;
                u.image_input = n("/usage/input_tokens_details/image_tokens")?;
                u.image_output = n("/usage/output_tokens_details/image_tokens")?;
                u.cached_image_input = n("/usage/input_tokens_details/cached_image_tokens")?;
                for (name, path) in [
                    (
                        "text_input_tokens",
                        "/usage/input_tokens_details/text_tokens",
                    ),
                    (
                        "text_output_tokens",
                        "/usage/output_tokens_details/text_tokens",
                    ),
                ] {
                    if let Some(count) = n(path)? {
                        u.quantities.insert(name.into(), count);
                    }
                }
            }
            Wire::Chat => {
                u.input = n("/usage/prompt_tokens")?;
                u.output = n("/usage/completion_tokens")?;
                u.cached = n("/usage/prompt_tokens_details/cached_tokens")?;
                u.reasoning = n("/usage/completion_tokens_details/reasoning_tokens")?;
                u.image_input = n("/usage/prompt_tokens_details/image_tokens")?;
                u.image_output = n("/usage/completion_tokens_details/image_tokens")?;
                u.cached_image_input = n("/usage/prompt_tokens_details/cached_image_tokens")?;
            }
            Wire::GenerateContent => {
                u.input = n("/usageMetadata/promptTokenCount")?;
                u.cached = n("/usageMetadata/cachedContentTokenCount")?;
                u.reasoning = n("/usageMetadata/thoughtsTokenCount")?;
                let candidates = n("/usageMetadata/candidatesTokenCount")?;
                u.output = match (candidates, u.reasoning) {
                    (Some(c), Some(r)) => Some(c.checked_add(r).ok_or_else(invalid)?),
                    (Some(c), None) if !self.config.capabilities.counters.reasoning => Some(c),
                    _ => None,
                };
                u.image_input = modality(v.pointer("/usageMetadata/promptTokensDetails"), "IMAGE")?;
                u.image_output =
                    modality(v.pointer("/usageMetadata/candidatesTokensDetails"), "IMAGE")?;
                u.cached_image_input =
                    modality(v.pointer("/usageMetadata/cacheTokensDetails"), "IMAGE")?;
            }
        }
        if let (Some(i), Some(o)) = (u.input, u.output) {
            u.validate_subsets(i, o)?;
        }
        if let Some(price) = &self.config.capabilities.limits.price
            && price.model == u.model
        {
            u.micro_usd = match price.cost(&u) {
                Ok(cost) => Some(cost),
                Err(Error::UsageUnknown) => None,
                Err(error) => return Err(error),
            };
        }
        Ok(u)
    }
}
fn modality(value: Option<&Value>, modality: &str) -> Result<Option<u64>> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let rows = value.as_array().ok_or_else(invalid)?;
    let mut total: Option<u64> = None;
    for row in rows.iter().filter(|r| r["modality"] == modality) {
        total = Some(
            total
                .unwrap_or(0)
                .checked_add(row["tokenCount"].as_u64().ok_or_else(invalid)?)
                .ok_or_else(invalid)?,
        );
    }
    Ok(total)
}
fn parse_call(id: Option<&str>, name: Option<&str>, arguments: Option<Value>) -> Result<ToolCall> {
    let (Some(id), Some(name), Some(arguments)) = (id, name, arguments) else {
        return Err(invalid());
    };
    if id.is_empty() || name.is_empty() || !arguments.is_object() {
        return Err(invalid());
    }
    Ok(ToolCall {
        id: id.into(),
        name: name.into(),
        arguments,
    })
}
fn safe_retry_after(value: &str) -> Option<String> {
    (value.parse::<u64>().is_ok() || httpdate::parse_http_date(value).is_ok())
        .then(|| value.to_owned())
}
fn retry_delay(value: &str) -> Option<u64> {
    value.parse().ok().or_else(|| {
        httpdate::parse_http_date(value).ok().map(|date| {
            date.duration_since(SystemTime::now())
                .unwrap_or_default()
                .as_secs()
        })
    })
}
fn classify(status: u16, value: &Value) -> ErrorClass {
    let error = &value["error"];
    let explicit_spending = [
        "insufficient_quota",
        "billing_hard_limit_reached",
        "billing_not_active",
        "credit_balance_too_low",
        "insufficient_credits",
        "spending_limit_exceeded",
    ];
    let codes = [
        error["code"].as_str(),
        error["type"].as_str(),
        error["metadata"]["limit_source"].as_str(),
    ];
    if status == 402
        || codes.into_iter().flatten().any(|c| {
            explicit_spending.contains(&c)
                || matches!(c, "openrouter_key_limit" | "openrouter_credits")
        })
        || error["details"].as_array().is_some_and(|rows| {
            rows.iter().any(|d| {
                matches!(
                    d["reason"].as_str(),
                    Some(
                        "BILLING_DISABLED"
                            | "BILLING_NOT_ACTIVE"
                            | "SPENDING_LIMIT_EXCEEDED"
                            | "INSUFFICIENT_CREDITS"
                    )
                )
            })
        })
    {
        return ErrorClass::SpendingLimit;
    }
    match status {
        401 => ErrorClass::Authentication,
        403 if matches!(error["status"].as_str(), Some("UNAUTHENTICATED")) => {
            ErrorClass::Authentication
        }
        400 if error["details"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|d| d["reason"] == "API_KEY_INVALID")) =>
        {
            ErrorClass::Authentication
        }
        400 | 403 | 404 | 405 | 413 | 415 | 422 => ErrorClass::InvalidRequest,
        429 => ErrorClass::RateLimit,
        300..400 => ErrorClass::Redirect,
        _ => ErrorClass::Unclassified,
    }
}
macro_rules! adapter {
    ($name:ident, $feature:literal, $wire:ident) => {
        #[cfg(feature = $feature)]
        #[derive(Debug)]
        pub struct $name(Adapter);
        #[cfg(feature = $feature)]
        impl $name {
            pub fn new(config: Config) -> Result<Self> {
                Adapter::new(config, Wire::$wire).map(Self)
            }
            pub fn capabilities(&self) -> &Capabilities {
                &self.0.config.capabilities
            }
        }
        #[cfg(feature = $feature)]
        impl ModelPort for $name {
            fn limits(&self) -> ModelLimits {
                self.0.config.capabilities.limits.clone()
            }
            fn input_upper_bound(&self, request: &PreparedModelRequest) -> Result<u64> {
                request.validate_for_send()?;
                // Size the largest feasible cap too: its decimal wire encoding
                // must fit before reservation, even when the prototype cap is 0.
                let limits = self.limits();
                let ceiling = limits.max_output.min(
                    limits
                        .context
                        .saturating_sub(self.0.config.input_token_upper_bound),
                );
                let body = self.0.body(
                    &self.0.config.run,
                    &request
                        .clone()
                        .with_max_output(request.max_output.max(ceiling)),
                )?;
                self.0.check_document_wire_bound(request, &body)?;
                Ok(self.0.config.input_token_upper_bound)
            }
            fn retry_policy(&self) -> RetryPolicy {
                RetryPolicy {
                    max_attempts: 1,
                    backoff: 0,
                    max_elapsed: self.limits().timeout,
                }
            }
            async fn invoke(
                &self,
                context: &ToolContext,
                request: PreparedModelRequest,
            ) -> Result<ExternalResult> {
                self.0.invoke(context, request).await
            }
            async fn lookup(
                &self,
                context: &ToolContext,
                request: PreparedModelRequest,
            ) -> Result<Lookup> {
                let body = self.0.body(&context.claim.run, &request)?;
                self.0.check_document_wire_bound(&request, &body)?;
                self.0.authorize_attachments(context, &request)?;
                Ok(Lookup::Unknown)
            }
        }
    };
}
adapter!(OpenAi, "openai", Responses);
adapter!(Gemini, "gemini", GenerateContent);
adapter!(OpenRouter, "openrouter", Chat);

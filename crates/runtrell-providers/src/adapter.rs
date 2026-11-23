// SPDX-License-Identifier: Apache-2.0
use crate::*;
use reqwest::{Client, Url, header::HeaderValue};
use runtrell::{attachments::ResolvedAttachment, *};
use serde_json::json;
use std::time::{Duration, Instant, SystemTime};
use std::{
    collections::BTreeMap,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Wire {
    Responses,
    GenerateContent,
    Chat,
}
#[derive(Debug)]
pub(crate) struct Adapter {
    pub(crate) config: Config,
    pub(crate) managed: Option<Managed>,

    client: Client,
    pub(crate) wire: Wire,
}
pub(crate) struct Managed {
    pub(crate) model: models::ResolvedModel,
    pub(crate) pricing: models::PricingSnapshot,
    credentials: CredentialResolver,
    reasoning: Option<String>,
}

impl std::fmt::Debug for Managed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Managed")
            .field("model", &self.model.id())
            .finish_non_exhaustive()
    }
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
    Json {
        value: Value,
        diagnostic: ProviderDiagnostic,
    },
    Failure(HttpFailure),
    Ambiguous(AmbiguousOutcome),
}

impl HttpResult {
    fn from_ambiguity(result: ExternalResult) -> Self {
        let ExternalResult::Ambiguous(outcome) = result else {
            unreachable!("expected the ambiguity constructor result");
        };
        Self::Ambiguous(outcome)
    }
}

pub(crate) struct HttpFailure {
    pub code: String,
    pub usage: Usage,
    pub retry_after: Option<u64>,
    pub diagnostic: ProviderDiagnostic,
}

impl From<HttpFailure> for ExternalResult {
    fn from(failure: HttpFailure) -> Self {
        Self::Failed {
            physical_evidence: None,
            code: failure.code,
            usage: failure.usage,
            retry_after: failure.retry_after,
            repair_hint: None,
            provider_diagnostic: Some(failure.diagnostic),
        }
    }
}

// A request requires its JSON body to execute. Polling this frame is the
// earliest point at which the transport can write payload bytes.
struct RequestBody {
    bytes: Option<bytes::Bytes>,
    grant: Option<ManagedSend>,
    released: Arc<AtomicBool>,
}

impl http_body::Body for RequestBody {
    type Data = bytes::Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<std::result::Result<http_body::Frame<Self::Data>, Self::Error>>> {
        let Some(bytes) = self.bytes.take() else {
            return Poll::Ready(None);
        };
        if self.grant.as_ref().is_some_and(|grant| !grant.begin_body()) {
            return Poll::Ready(Some(Err(std::io::Error::other("send grant revoked"))));
        }
        self.released.store(true, Ordering::Release);
        Poll::Ready(Some(Ok(http_body::Frame::data(bytes))))
    }

    fn size_hint(&self) -> http_body::SizeHint {
        http_body::SizeHint::with_exact(self.bytes.as_ref().map_or(0, |bytes| bytes.len() as u64))
    }
}
fn safe_field(value: &Value) -> Option<SafeProviderField> {
    match value {
        Value::String(s) => SafeProviderField::try_from(s.clone()).ok(),
        Value::Number(n) => SafeProviderField::try_from(n.to_string()).ok(),
        _ => None,
    }
}
fn gemini_field(error: &Value) -> Option<SafeProviderField> {
    error["details"].as_array()?.iter().find_map(|detail| {
        (detail["@type"] == "type.googleapis.com/google.rpc.BadRequest")
            .then(|| safe_field(&detail["fieldViolations"][0]["field"]))
            .flatten()
    })
}
fn diagnostic(
    phase: ProviderFailurePhase,
    send_certainty: ProviderSendCertainty,
    http_status: Option<u16>,
    parsed: &Value,
    wire: Wire,
) -> ProviderDiagnostic {
    let error = &parsed["error"];
    let structured = error.is_object();
    ProviderDiagnostic {
        phase,
        send_certainty,
        http_status,
        provider_type: structured
            .then(|| match wire {
                Wire::GenerateContent => safe_field(&error["status"]),
                _ => safe_field(&error["type"]),
            })
            .flatten(),
        provider_code: structured.then(|| safe_field(&error["code"])).flatten(),
        provider_param: structured
            .then(|| match wire {
                Wire::GenerateContent => gemini_field(error),
                _ => safe_field(&error["param"]),
            })
            .flatten(),
        request_id: None,
    }
}
fn invalid() -> Error {
    Error::Invalid("provider request or configuration".into())
}

// Request-local reversible aliases. History is included because a past call can
// refer to a tool no longer advertised in the current catalog.
struct ToolNames {
    to_wire: BTreeMap<String, String>,
    to_logical: BTreeMap<String, String>,
}
impl ToolNames {
    fn new(request: &PreparedModelRequest, wire: Wire) -> Result<Self> {
        let mut names = Self {
            to_wire: BTreeMap::new(),
            to_logical: BTreeMap::new(),
        };
        for name in request.tools.iter().map(|t| t.name.as_str()).chain(
            request
                .messages
                .iter()
                .filter(|m| m.role != "provider")
                .filter_map(|m| m.tool.as_deref()),
        ) {
            let alias = if wire == Wire::GenerateContent
                || (!name.is_empty()
                    && name.len() <= 64
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')))
            {
                name.to_owned()
            } else {
                digest(&json!(name))
            };
            if names
                .to_logical
                .get(&alias)
                .is_some_and(|previous| previous != name)
            {
                return Err(Error::Invalid("provider tool name collision".into()));
            }
            names.to_logical.insert(alias.clone(), name.to_owned());
            names.to_wire.insert(name.to_owned(), alias);
        }
        Ok(names)
    }
    fn wire(&self, name: &str) -> Result<&str> {
        self.to_wire
            .get(name)
            .map(String::as_str)
            .ok_or_else(invalid)
    }
}
impl Adapter {
    #[cfg(test)]
    pub(crate) fn new(config: Config, wire: Wire) -> Result<Self> {
        if config.key.0.is_empty()
            || config.endpoint.contains(&config.key.0)
            || HeaderValue::from_str(&config.key.0).is_err()
        {
            return Err(invalid());
        }
        Self::build(config, wire, None)
    }

    pub(crate) fn managed(
        config: Config,
        wire: Wire,
        model: models::ResolvedModel,
        credentials: CredentialResolver,
        reasoning: Option<String>,
    ) -> Result<Self> {
        Self::build(
            config,
            wire,
            Some(Managed {
                pricing: model.pricing(),
                model,
                credentials,
                reasoning,
            }),
        )
    }

    fn build(config: Config, wire: Wire, managed: Option<Managed>) -> Result<Self> {
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
            || l.model.is_empty()
            || l.context == 0
            || l.max_output == 0
            || l.min_max_output_tokens == 0
            || l.min_max_output_tokens > l.max_output
            || l.max_output > l.context
            || l.price.as_ref().is_some_and(|p| p.model != l.model)
            || l.timeout == 0
            || config.retry_policy.max_attempts == 0
            || config.retry_policy.backoff == 0
            || config.retry_policy.max_elapsed == 0
            || config.max_request_bytes == 0
            || config.max_response_bytes == 0
            || config.input_token_upper_bound == 0
            || config.input_token_upper_bound > l.context
            || (managed.is_none() && config.input_token_upper_bound == l.context)
            || (config.capabilities.parallel_tool_calls && !config.capabilities.tool_calling)
        {
            return Err(invalid());
        }
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .referer(false)
            .timeout(Duration::from_millis(l.timeout))
            .build()
            .map_err(|_| invalid())?;
        Ok(Self {
            config,
            managed,
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
        if self.managed.is_some()
            && self.wire == Wire::Responses
            && !state.content.as_array().is_some_and(|items| {
                items.iter().all(|item| {
                    matches!(
                        item["type"].as_str(),
                        Some("message" | "function_call" | "reasoning")
                    )
                })
            })
        {
            return Err(Error::PermissionDenied);
        }
        Ok(state)
    }
    fn background(&self) -> bool {
        self.managed
            .as_ref()
            .is_some_and(|managed| managed.model.background())
    }

    pub(crate) fn body(&self, run: &RunHandle, request: &PreparedModelRequest) -> Result<Value> {
        self.model_body(run, request, None)
    }

    fn model_body(
        &self,
        run: &RunHandle,
        request: &PreparedModelRequest,
        resolved: Option<&[ResolvedAttachment]>,
    ) -> Result<Value> {
        if *run != self.config.run {
            return Err(Error::PermissionDenied);
        }
        if self.background() && (!request.images.is_empty() || !request.attachments.is_empty()) {
            return Err(Error::Unsupported(
                "attachments on background responses".into(),
            ));
        }
        let names = ToolNames::new(request, self.wire)?;
        let c = &self.config.capabilities;
        let l = &c.limits;
        if request.model != l.model
            || request.max_output < l.min_max_output_tokens
            || request.max_output > l.max_output
            || (self.managed.is_none()
                && self
                    .config
                    .input_token_upper_bound
                    .checked_add(request.max_output)
                    .is_none_or(|n| n > l.context))
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
        self.check_input_attachments(request.attachments(), &request.images)?;
        let mut messages = Vec::new();
        let mut systems = Vec::new();
        let mut slots = request.media_slots()?.into_iter();
        let initial = slots.next().ok_or_else(invalid)?;
        if !initial.is_empty() {
            messages.push(self.media_item(request, &initial, resolved)?);
        }
        let mut replaying = false;
        for (m, after) in request.messages.iter().zip(slots) {
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
                        json!({"type":"function_call","call_id":id,"name":names.wire(m.tool.as_deref().ok_or_else(invalid)?)?,"arguments":m.data.to_string()})
                    } else {
                        json!({"role":m.role,"content":[{"type":if m.role == "assistant" {"output_text"} else {"input_text"},"text":m.text}]})
                    }
                }
                Wire::Chat => {
                    if m.role == "tool" {
                        json!({"role":"tool","tool_call_id":m.call_id.as_ref().ok_or_else(invalid)?,"content":output})
                    } else if let Some(id) = &m.call_id {
                        json!({"role":"assistant","tool_calls":[{"id":id,"type":"function","function":{"name":names.wire(m.tool.as_deref().ok_or_else(invalid)?)?,"arguments":m.data.to_string()}}]})
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
            if !after.is_empty() {
                messages.push(self.media_item(request, &after, resolved)?);
            }
        }
        let functions: Vec<_> = request.tools.iter().map(|t| Ok(match self.wire {
            Wire::Responses => json!({"type":"function","name":names.wire(&t.name)?,"description":t.capability,"parameters":t.input_schema,"strict":false}),
            Wire::Chat => json!({"type":"function","function":{"name":names.wire(&t.name)?,"description":t.capability,"parameters":t.input_schema}}),
            Wire::GenerateContent => json!({"name":t.name,"description":t.capability,"parametersJsonSchema":t.input_schema}),
        })).collect::<Result<_>>()?;
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
        if let Some(managed) = &self.managed {
            if self.wire == Wire::Responses {
                body["service_tier"] = json!("default");
                body["truncation"] = json!("disabled");
                if managed.model.background() {
                    body["background"] = json!(true);
                }
                let ttl = match managed.model.cache_retention_seconds() {
                    Some(1800) => "30m",
                    Some(43200) => "12h",
                    _ => return Err(Error::ReservationDenied),
                };
                body["prompt_cache_options"] = json!({"mode":"implicit","ttl":ttl});
                if let Some(mode) = &managed.reasoning {
                    body["reasoning"] = json!({"effort":mode});
                }
            }
            if self.wire == Wire::GenerateContent {
                body["serviceTier"] = json!("standard");
                body["generationConfig"]["thinkingConfig"] = json!({"thinkingBudget":0});
            }
            if self.wire == Wire::Chat {
                body["provider"] = json!({"only":managed.model.endpoint_tags(),"allow_fallbacks":false,"require_parameters":true});
            }
        }
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
        let prototype = self.body(&context.claim.run, &request)?;
        self.check_media_wire_bound(&request, &prototype)?;
        let input_bound = self.model_input_bound(&prototype, &request)?;
        let resolved = match self.resolve_documents(context, &request).await {
            Ok(resolved) => resolved,
            Err(_) => {
                return Ok(ExternalResult::Failed {
                    repair_hint: None,
                    physical_evidence: None,
                    code: "ATTACHMENT_NOT_SENT".into(),
                    usage: Usage {
                        model: request.model.clone(),
                        ..Usage::non_billable(NonBillableReason::NotSent)
                    },
                    retry_after: None,
                    provider_diagnostic: Some(diagnostic(
                        ProviderFailurePhase::PreSend,
                        ProviderSendCertainty::DefinitelyNotSent,
                        None,
                        &Value::Null,
                        self.wire,
                    )),
                });
            }
        };
        let body = if request.images.is_empty() && resolved.is_empty() {
            prototype
        } else {
            self.model_body(&context.claim.run, &request, Some(&resolved))?
        };
        match self
            .send_at_with_usage(
                context,
                &self.config.endpoint,
                body,
                &resolved,
                Some(input_bound),
                |_, parsed| self.normalized_usage(parsed),
            )
            .await?
        {
            HttpResult::Json {
                value: parsed,
                diagnostic,
            } => {
                if self.background() && self.background_id(&parsed).is_none() {
                    return Ok(ExternalResult::ambiguous(
                        AmbiguityEvidence::Provider(diagnostic),
                        AmbiguityReason::ResponseIdentityUnverified,
                    ));
                }
                Ok(self.submit_result(&parsed, &request).unwrap_or_else(|_| {
                    if self.background()
                        && !matches!(parsed["status"].as_str(), Some("completed" | "incomplete"))
                    {
                        ExternalResult::ambiguous(
                            AmbiguityEvidence::Provider(diagnostic),
                            AmbiguityReason::ResponseIdentityUnverified,
                        )
                    } else {
                        HttpFailure {
                            code: "PROVIDER_RESPONSE_INVALID".into(),
                            usage: self.normalized_usage(&parsed),
                            retry_after: None,
                            diagnostic,
                        }
                        .into()
                    }
                }))
            }
            HttpResult::Failure(result) => Ok(result.into()),
            HttpResult::Ambiguous(outcome) => Ok(ExternalResult::Ambiguous(outcome)),
        }
    }
    fn submit_result(
        &self,
        value: &Value,
        request: &PreparedModelRequest,
    ) -> Result<ExternalResult> {
        if !self.background() {
            return self.reply(value, request);
        }
        let id = self.background_id(value).ok_or_else(invalid)?;
        if matches!(value["status"].as_str(), Some("queued" | "in_progress")) {
            return Ok(ExternalResult::Accepted {
                external_id: id.into(),
            });
        }
        self.background_terminal(value, request)
    }

    fn background_id<'a>(&self, value: &'a Value) -> Option<&'a str> {
        let id = value["id"].as_str()?;
        if !id.starts_with("resp_")
            || id.len() <= "resp_".len()
            || id.len() > 256
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            || value["object"] != "response"
            || value["background"] != true
            || value["model"] != self.config.capabilities.limits.model
            || value["service_tier"] != "default"
        {
            return None;
        }
        Some(id)
    }

    fn background_terminal(
        &self,
        value: &Value,
        request: &PreparedModelRequest,
    ) -> Result<ExternalResult> {
        match value["status"].as_str() {
            Some("completed" | "incomplete") => self.reply(value, request),
            Some("failed" | "cancelled") => Ok(ExternalResult::Failed {
                code: "BACKGROUND_RESPONSE_FAILED".into(),
                usage: self.normalized_usage(value),
                repair_hint: None,
                physical_evidence: None,
                retry_after: None,
                provider_diagnostic: None,
            }),
            _ => Err(invalid()),
        }
    }

    async fn lookup(&self, context: &ToolContext, request: PreparedModelRequest) -> Result<Lookup> {
        let body = self.body(&context.claim.run, &request)?;
        self.check_media_wire_bound(&request, &body)?;
        self.authorize_attachments(context, &request)?;
        let Some(managed) = self.managed.as_ref().filter(|_| self.background()) else {
            return Ok(Lookup::Unknown);
        };
        let Some(id) = context
            .managed_send
            .as_ref()
            .and_then(|grant| grant.poll_id(context, &managed.pricing))
        else {
            return Ok(Lookup::Unknown);
        };
        let value = match self.poll_response(context, managed, id).await {
            Ok(value) => value,
            Err(error) => return Ok(Lookup::Unavailable(error)),
        };
        if self.background_id(&value) != Some(id) {
            return Ok(Lookup::Unavailable(LookupFailure::IdentityMismatch));
        }
        if matches!(value["status"].as_str(), Some("queued" | "in_progress")) {
            return Ok(Lookup::Pending);
        }
        Ok(self.background_terminal(&value, &request).map_or(
            Lookup::Unavailable(LookupFailure::ResponseDecode),
            Lookup::Found,
        ))
    }

    async fn poll_response(
        &self,
        context: &ToolContext,
        managed: &Managed,
        id: &str,
    ) -> std::result::Result<Value, LookupFailure> {
        let ApiKey(key, _) =
            (managed.credentials)(managed.model.id()).map_err(|_| LookupFailure::Configuration)?;
        let timeout = self
            .request_timeout(context)
            .map_err(|_| LookupFailure::Transport)?;
        if key.is_empty() || self.config.endpoint.contains(&key) || id.contains(&key) {
            return Err(LookupFailure::Configuration);
        }
        let mut authorization = HeaderValue::from_str(&format!("Bearer {key}"))
            .map_err(|_| LookupFailure::Configuration)?;
        authorization.set_sensitive(true);
        let mut url =
            Url::parse(&self.config.endpoint).map_err(|_| LookupFailure::Configuration)?;
        url.path_segments_mut()
            .map_err(|_| LookupFailure::Configuration)?
            .push(id);
        let mut response = self
            .client
            .get(url)
            .timeout(timeout)
            .header("authorization", authorization)
            .send()
            .await
            .map_err(|_| LookupFailure::Transport)?;
        if !response.status().is_success() {
            return Err(LookupFailure::Rejected(response.status().as_u16()));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| LookupFailure::ResponseRead)?
        {
            if chunk.len() > self.config.max_response_bytes.saturating_sub(bytes.len()) {
                return Err(LookupFailure::ResponseByteLimit);
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| LookupFailure::ResponseDecode)
    }

    #[cfg(feature = "gemini")]
    pub(crate) async fn send(&self, context: &ToolContext, body: Value) -> Result<HttpResult> {
        self.send_at_with_usage(
            context,
            &self.config.endpoint,
            body,
            &[],
            None,
            |_, parsed| self.normalized_usage(parsed),
        )
        .await
    }

    pub(crate) fn normalized_usage(&self, parsed: &Value) -> Usage {
        self.usage(parsed).unwrap_or_else(|_| Usage {
            model: self.config.capabilities.limits.model.clone(),
            issue: Some(UsageIssue::Invalid),
            ..Usage::default()
        })
    }

    // The same input fields go to the token-count endpoint and into the
    // estimate. Borrow the payload so counting need not clone media data.
    fn responses_input(body: &Value) -> BTreeMap<&str, &Value> {
        [
            "model",
            "input",
            "tools",
            "text",
            "parallel_tool_calls",
            "reasoning",
            "truncation",
        ]
        .into_iter()
        .filter_map(|key| Some((key, body.get(key)?)))
        .collect()
    }

    fn model_input_bound(&self, body: &Value, request: &PreparedModelRequest) -> Result<u64> {
        let cap = self.config.input_token_upper_bound;
        if !self.counts_responses_input() {
            return Ok(cap);
        }
        let items = |key: &str| body[key].as_array().map_or(0, Vec::len) as u64;
        let bytes = serde_json::to_vec(&Self::responses_input(body))
            .map_err(|_| invalid())?
            .len() as u64;
        // The prototype preserves item framing but omits media transport. Use
        // the fixed lengths with preprocessing headroom. This estimate is
        // enforced against the provider's count before paid inference.
        const PREPROCESSING_TOKENS_PER_MEDIA: u64 = 4096;
        let media = request
            .images
            .iter()
            .map(|image| image.bytes())
            .chain(request.attachments().iter().map(|input| input.bytes()))
            .fold(0u64, |total, bytes| {
                total.saturating_add(
                    bytes
                        .div_ceil(3)
                        .saturating_mul(4)
                        .saturating_add(PREPROCESSING_TOKENS_PER_MEDIA),
                )
            });
        Ok(cap.min(
            bytes
                .saturating_add(256)
                .saturating_add(
                    items("input")
                        .saturating_add(items("tools"))
                        .saturating_mul(32),
                )
                .saturating_add(media),
        ))
    }
    pub(crate) fn managed_quote(&self, body: &Value, input_bound: Option<u64>) -> Result<Amount> {
        let managed = self.managed.as_ref().ok_or(Error::ReservationDenied)?;
        if managed.model.image_reservation().is_some() {
            return managed.pricing.estimate_image(
                body["size"].as_str().ok_or(Error::ReservationDenied)?,
                body["quality"].as_str().ok_or(Error::ReservationDenied)?,
                body["prompt"]
                    .as_str()
                    .ok_or(Error::ReservationDenied)?
                    .len(),
                body["images"]
                    .as_array()
                    .map_or(0, |refs| refs.len() as u64),
            );
        }
        let output = body["max_output_tokens"]
            .as_u64()
            .or_else(|| body["generationConfig"]["maxOutputTokens"].as_u64())
            .or_else(|| body["max_tokens"].as_u64())
            .unwrap_or(self.config.capabilities.limits.max_output);
        managed.pricing.estimate(
            input_bound.unwrap_or(self.config.input_token_upper_bound),
            output,
        )
    }

    fn counts_responses_input(&self) -> bool {
        self.managed
            .as_ref()
            .is_some_and(|managed| managed.model.api_family() == "openai_responses")
    }

    /// Free Responses preflight over the same complete input. The supported
    /// fields below include the output schema and the tool/message framing.
    async fn count_responses_input(
        &self,
        endpoint: &str,
        body: &Value,
        authorization: HeaderValue,
        timeout: Duration,
    ) -> Option<u64> {
        let input = Self::responses_input(body);
        let mut response = self
            .client
            .post(format!("{endpoint}/input_tokens"))
            .timeout(timeout)
            .header("authorization", authorization)
            .header("content-type", "application/json")
            .body(serde_json::to_vec(&input).ok()?)
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?;
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.ok()? {
            if bytes.len().checked_add(chunk.len())? > 4096 {
                return None;
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes).ok()?;
        (value["object"] == "response.input_tokens").then_some(())?;
        value["input_tokens"].as_u64()
    }

    pub(crate) fn unsent(&self, code: &str, reason: NonBillableReason) -> HttpResult {
        HttpResult::Failure(HttpFailure {
            code: code.into(),
            usage: Usage {
                model: self.config.capabilities.limits.model.clone(),
                ..Usage::non_billable(reason)
            },
            retry_after: None,
            diagnostic: diagnostic(
                ProviderFailurePhase::PreSend,
                ProviderSendCertainty::DefinitelyNotSent,
                None,
                &Value::Null,
                self.wire,
            ),
        })
    }

    fn responses_envelope_status(value: &Value) -> Option<u16> {
        let error = &value["error"];
        match (error["type"].as_str(), error["code"].as_str()) {
            (_, Some("insufficient_quota" | "billing_hard_limit_reached")) => Some(429),
            (Some("invalid_request_error"), _) => Some(400),
            (Some("authentication_error"), _) => Some(401),
            (Some("permission_error"), _) => Some(403),
            (Some("rate_limit_error"), _) => Some(429),
            _ => None,
        }
    }

    fn rejects_without_execution(&self, status: u16, effective_status: u16, value: &Value) -> bool {
        if [
            "usage",
            "usageMetadata",
            "svg_usage",
            "choices",
            "output",
            "candidates",
            "data",
            "id",
            "responseId",
            "promptFeedback",
        ]
        .iter()
        .any(|key| !value[key].is_null())
        {
            return false;
        }
        // Authentication and access checks reject before execution, even if
        // the body cannot be decoded. Other statuses need a matching envelope.
        if matches!(status, 401 | 403) && value.is_null() {
            return true;
        }
        if !matches!(
            effective_status,
            400 | 401 | 402 | 403 | 404 | 413 | 422 | 429
        ) || (!(200..300).contains(&status) && status != effective_status)
        {
            return false;
        }
        let error = &value["error"];
        match self.wire {
            Wire::Responses => {
                // The Responses and image wires use textual error codes.
                error.is_object()
                    && (!(200..300).contains(&status)
                        || Self::responses_envelope_status(value) == Some(effective_status))
            }
            Wire::Chat => {
                // A 2xx error on this wire can arrive during generation.
                !(200..300).contains(&status)
                    && error["code"].as_u64() == Some(u64::from(effective_status))
                    && error["metadata"]["provider_name"].is_null()
                    && error["metadata"]["raw"].is_null()
                    && error["metadata"]["upstream_id"].is_null()
            }
            Wire::GenerateContent => {
                let expected = match effective_status {
                    400 => "INVALID_ARGUMENT",
                    401 => "UNAUTHENTICATED",
                    403 => "PERMISSION_DENIED",
                    404 => "NOT_FOUND",
                    429 => "RESOURCE_EXHAUSTED",
                    _ => return false,
                };
                error["code"].as_u64() == Some(u64::from(effective_status))
                    && (error["status"] == expected
                        || (effective_status == 400 && error["status"] == "FAILED_PRECONDITION"))
            }
        }
    }

    fn request_timeout(&self, context: &ToolContext) -> Result<Duration> {
        let mut timeout = Duration::from_millis(self.config.capabilities.limits.timeout);
        if let Some(deadline) = context.deadline {
            let now = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_err(|_| Error::Deadline)?
                .as_millis();
            // `deadline` is Unix milliseconds (Clock epoch).
            let now = u64::try_from(now).map_err(|_| Error::Deadline)?;
            if deadline <= now {
                return Err(Error::Deadline);
            }
            timeout = timeout.min(Duration::from_millis(deadline - now));
        }
        Ok(timeout)
    }

    /// Returns `Err` only before submitting the HTTP request.
    pub(crate) async fn send_at_with_usage<F: Fn(u16, &Value) -> Usage + Send>(
        &self,
        context: &ToolContext,
        endpoint: &str,
        body: Value,
        sensitive: &[ResolvedAttachment],
        input_bound: Option<u64>,
        normalize: F,
    ) -> Result<HttpResult> {
        if context.claim.run != self.config.run {
            return Err(Error::PermissionDenied);
        }
        let bytes = serde_json::to_vec(&body).map_err(|_| invalid())?;
        if bytes.len() > self.config.max_request_bytes {
            return Ok(self.unsent("REQUEST_BYTE_LIMIT", NonBillableReason::NotSent));
        }
        let mut timeout = self.request_timeout(context)?;
        let (key, authorized) = if let Some(managed) = &self.managed {
            let quote = self.managed_quote(&body, input_bound)?;
            let grant = context
                .managed_send
                .as_ref()
                .ok_or(Error::ReservationDenied)?;
            grant.check(context, &managed.pricing, quote)?;
            let key = match (managed.credentials)(managed.model.id()) {
                Ok(key) => key,
                Err(Error::PermissionDenied) => return Err(Error::PermissionDenied),
                Err(_) => {
                    return Ok(self.unsent(
                        "CREDENTIAL_UNAVAILABLE",
                        NonBillableReason::RetryableNotSent,
                    ));
                }
            };
            if self.wire == Wire::Chat {
                let payer = match managed.model.billing_mode() {
                    Some("routed") => OpenRouterPayer::Credits,
                    Some("byok") => OpenRouterPayer::Byok,
                    _ => return Err(Error::ReservationDenied),
                };
                if key.1 != Some(payer) {
                    return Ok(self.unsent(
                        "OPENROUTER_CREDENTIAL_PAYER_MISMATCH",
                        NonBillableReason::NotSent,
                    ));
                }
            }
            (key, Some((grant, &managed.pricing, quote)))
        } else {
            #[cfg(test)]
            {
                (self.config.key.clone(), None)
            }
            #[cfg(not(test))]
            {
                return Err(Error::ReservationDenied);
            }
        };
        if key.0.is_empty() || endpoint.contains(&key.0) {
            return Err(invalid());
        }
        let (header, value) = if self.wire == Wire::GenerateContent {
            ("x-goog-api-key", HeaderValue::from_str(&key.0))
        } else {
            (
                "authorization",
                HeaderValue::from_str(&format!("Bearer {}", key.0)),
            )
        };
        let Ok(mut value) = value else {
            return Ok(self.unsent("REQUEST_BUILDER", NonBillableReason::NotSent));
        };
        value.set_sensitive(true);
        if self.counts_responses_input() {
            let bound = input_bound.ok_or(Error::ReservationDenied)?;
            let start = Instant::now();
            let count = self
                .count_responses_input(endpoint, &body, value.clone(), timeout)
                .await;
            timeout = timeout.saturating_sub(start.elapsed());
            let code = match count {
                Some(count) if count > self.config.input_token_upper_bound => {
                    Some("CONTEXT_CAP_EXCEEDED")
                }
                Some(count) if count > bound => Some("INPUT_TOKEN_ESTIMATE_EXCEEDED"),
                Some(_) if !timeout.is_zero() => None,
                _ => Some("INPUT_TOKEN_COUNT_UNAVAILABLE"),
            };
            if let Some(code) = code {
                return Ok(self.unsent(code, NonBillableReason::NotSent));
            }
        }
        let released = Arc::new(AtomicBool::new(false));
        let request = self
            .client
            .post(endpoint)
            .timeout(timeout)
            .header(header, value)
            .header("content-type", "application/json")
            .body(reqwest::Body::wrap(RequestBody {
                bytes: Some(bytes.into()),
                grant: authorized.map(|(grant, _, _)| grant.clone()),
                released: released.clone(),
            }))
            .build();
        let Ok(request) = request else {
            return Ok(self.unsent("REQUEST_BUILDER", NonBillableReason::NotSent));
        };
        if let Some((grant, price, quote)) = authorized {
            grant.authorize_tracked(context, price, quote)?;
        }
        let mut response = match self.client.execute(request).await {
            Ok(response) => response,
            Err(e)
                if e.is_connect()
                    || e.is_builder()
                    || authorized.map_or_else(
                        || !released.load(Ordering::Acquire),
                        |(grant, _, _)| grant.revoke_if_not_sent(),
                    ) =>
            {
                return Ok(self.unsent(
                    "CONNECTION_NOT_ESTABLISHED",
                    NonBillableReason::RetryableNotSent,
                ));
            }
            Err(_) => {
                return Ok(HttpResult::from_ambiguity(ExternalResult::ambiguous(
                    AmbiguityEvidence::Provider(diagnostic(
                        ProviderFailurePhase::Send,
                        ProviderSendCertainty::Unknown,
                        None,
                        &Value::Null,
                        self.wire,
                    )),
                    AmbiguityReason::TransportAfterPossibleWrite,
                )));
            }
        };
        let status = response.status().as_u16();
        let request_id = response
            .headers()
            .get("x-request-id")
            .and_then(|h| h.to_str().ok())
            .and_then(|id| SafeProviderField::try_from(id.to_owned()).ok());
        let response_diagnostic = |phase, parsed: &Value| {
            let mut diagnostic = diagnostic(
                phase,
                ProviderSendCertainty::Sent,
                Some(status),
                parsed,
                self.wire,
            );
            diagnostic.request_id = request_id.clone();
            for field in [
                &mut diagnostic.provider_type,
                &mut diagnostic.provider_code,
                &mut diagnostic.provider_param,
                &mut diagnostic.request_id,
            ] {
                if field.as_deref().is_some_and(|field| field.contains(&key.0)) {
                    *field = None;
                }
            }
            diagnostic
        };
        let retry_header_present = response.headers().contains_key("retry-after");
        let retry_header = response
            .headers()
            .get("retry-after")
            .and_then(|h| h.to_str().ok())
            .and_then(safe_retry_after);
        let mut bytes = Vec::new();
        let read_failure = loop {
            match response.chunk().await {
                Ok(Some(chunk)) => {
                    if chunk.len() > self.config.max_response_bytes.saturating_sub(bytes.len()) {
                        break Some(ProviderFailurePhase::ResponseByteLimit);
                    }
                    bytes.extend_from_slice(&chunk);
                }
                Ok(None) => break None,
                Err(_) => break Some(ProviderFailurePhase::ResponseRead),
            }
        };
        let mut parsed: Value = if let Some(phase) = read_failure {
            if (400..500).contains(&status) || (500..600).contains(&status) {
                let mut usage = normalize(status, &Value::Null);
                if self.rejects_without_execution(status, status, &Value::Null) {
                    usage = Usage::non_billable(NonBillableReason::RejectedBeforeExecution);
                }
                return Ok(HttpResult::Failure(HttpFailure {
                    code: serde_json::to_string(&Failure {
                        class: classify(status, &Value::Null),
                        status,
                        retry_after: retry_header,
                    })
                    .map_err(|_| Error::Storage)?,
                    usage,
                    retry_after: None,
                    diagnostic: response_diagnostic(phase, &Value::Null),
                }));
            }
            let evidence = AmbiguityEvidence::Provider(response_diagnostic(phase, &Value::Null));
            return Ok(HttpResult::from_ambiguity(match phase {
                ProviderFailurePhase::ResponseByteLimit => {
                    ExternalResult::ambiguous(evidence, AmbiguityReason::ResponseByteLimit)
                }
                _ => ExternalResult::ambiguous(evidence, AmbiguityReason::ResponseLost),
            }));
        } else {
            match serde_json::from_slice(&bytes) {
                Ok(v) => v,
                Err(_) if !(200..300).contains(&status) => Value::Null,
                Err(_) => {
                    return Ok(HttpResult::from_ambiguity(ExternalResult::ambiguous(
                        AmbiguityEvidence::Provider(response_diagnostic(
                            ProviderFailurePhase::ResponseDecode,
                            &Value::Null,
                        )),
                        AmbiguityReason::ResponseDecode,
                    )));
                }
            }
        };
        crate::documents::redact_echoes(&mut parsed, sensitive);
        if self.wire == Wire::Chat
            && parsed["error"].is_null()
            && let Some(error) = parsed
                .pointer("/choices/0/error")
                .filter(|error| error.is_object())
        {
            parsed["error"] = error.clone();
        }
        if !(200..300).contains(&status) || !parsed["error"].is_null() {
            let effective_status = parsed["error"]["code"]
                .as_u64()
                .and_then(|n| u16::try_from(n).ok())
                .filter(|n| (400..600).contains(n))
                .or_else(|| {
                    (self.wire == Wire::Responses && (200..300).contains(&status))
                        .then(|| Self::responses_envelope_status(&parsed))
                        .flatten()
                })
                .unwrap_or(status);
            let mut class = classify(effective_status, &parsed);
            let retryable = self.wire == Wire::Responses
                && match status {
                    429 => {
                        parsed["error"]["type"] == "rate_limit_error"
                            && parsed["error"]["code"] == "slow_down"
                    }
                    503 => {
                        parsed["error"]["type"] == "service_unavailable_error"
                            && parsed["error"]["code"] == "server_is_overloaded"
                    }
                    _ => false,
                }
                && class != ErrorClass::SpendingLimit;
            if retryable {
                class = ErrorClass::Retryable;
            }
            let failure = Failure {
                class,
                status: effective_status,
                retry_after: retry_header.clone(),
            };
            let delay = retryable.then(|| match retry_header_present {
                true => retry_header
                    .as_deref()
                    .and_then(retry_delay)
                    .unwrap_or(u64::MAX),
                false => 0,
            });
            let usage = if self.rejects_without_execution(status, effective_status, &parsed) {
                Usage {
                    model: self.config.capabilities.limits.model.clone(),
                    ..Usage::non_billable(NonBillableReason::RejectedBeforeExecution)
                }
            } else {
                normalize(status, &parsed)
            };
            return Ok(HttpResult::Failure(HttpFailure {
                code: serde_json::to_string(&failure)
                    .unwrap_or_else(|_| "PROVIDER_HTTP_ERROR".into()),
                usage,
                retry_after: delay,
                diagnostic: response_diagnostic(ProviderFailurePhase::HttpError, &parsed),
            }));
        }
        Ok(HttpResult::Json {
            value: parsed,
            diagnostic: response_diagnostic(ProviderFailurePhase::ResponseDecode, &Value::Null),
        })
    }
    fn reply(&self, v: &Value, request: &PreparedModelRequest) -> Result<ExternalResult> {
        let names = ToolNames::new(request, self.wire)?;
        let usage = self.normalized_usage(v);
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
                if !choice.is_object() {
                    return Err(invalid());
                }
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
                if !candidate.is_object() && v["promptFeedback"]["blockReason"].is_null() {
                    return Err(invalid());
                }
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
        for call in &mut calls {
            if let Some(logical) = names.to_logical.get(&call.name) {
                call.name = logical.clone();
            } else {
                partial_calls = true;
            }
        }
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
        let model_field = if self.wire == Wire::GenerateContent {
            "modelVersion"
        } else {
            "model"
        };
        let reported_model = v[model_field].as_str();
        let mut u = Usage {
            model: reported_model
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
                if self.managed.is_some() {
                    for (name, path) in [
                        (
                            models::CACHE_WRITE,
                            "/usage/input_tokens_details/cache_write_tokens",
                        ),
                        (
                            models::CACHE_WRITE_30M,
                            "/usage/input_tokens_details/cache_write_tokens_details/ttl_30m_tokens",
                        ),
                        (
                            models::CACHE_WRITE_12H,
                            "/usage/input_tokens_details/cache_write_tokens_details/ttl_12h_tokens",
                        ),
                    ] {
                        if let Some(count) = n(path)? {
                            u.quantities.insert(name.into(), count);
                        }
                    }
                }
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
                if self.managed.is_some() {
                    // The cataloged Gemini wire disables thinking.
                    u.reasoning = u.reasoning.or(Some(0));
                    if u.reasoning.is_some_and(|n| n > 0)
                        || n("/usageMetadata/toolUsePromptTokenCount")?.is_some_and(|n| n > 0)
                    {
                        u.unknown_reason = Some("MANAGED_ROUTE_MISMATCH".into());
                    }
                }
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
            if i.checked_add(o).is_none() {
                u.issue = Some(UsageIssue::ArithmeticOverflow);
                return Ok(u);
            }
            if u.validate_subsets(i, o).is_err() {
                u.issue = Some(UsageIssue::Invalid);
                return Ok(u);
            }
        }
        if self.managed.is_some() {
            let total_path = match self.wire {
                Wire::Responses | Wire::Chat => "/usage/total_tokens",
                Wire::GenerateContent => "/usageMetadata/totalTokenCount",
            };
            if let (Some(total), Some(input), Some(output)) = (n(total_path)?, u.input, u.output)
                && input.checked_add(output) != Some(total)
            {
                u.issue = Some(UsageIssue::Invalid);
                return Ok(u);
            }
        }
        if let Some(managed) = &self.managed {
            let route_mismatch = match self.wire {
                Wire::Responses => {
                    v["service_tier"] != "default"
                        || (self.counts_responses_input()
                            && u.input
                                .is_some_and(|n| n > self.config.input_token_upper_bound))
                }
                Wire::GenerateContent => {
                    v["usageMetadata"]["serviceTier"]
                        .as_str()
                        .is_some_and(|tier| !matches!(tier, "standard" | "unspecified"))
                        || v["candidates"].as_array().is_some_and(|c| c.len() > 1)
                }
                Wire::Chat => {
                    v["service_tier"]
                        .as_str()
                        .is_some_and(|tier| tier != "default")
                        || v["provider"].as_str().is_none_or(|provider| {
                            managed
                                .pricing
                                .record_openrouter_charge(&mut u, provider, &v["usage"])
                                .is_err()
                        })
                }
            };
            if reported_model.is_none() || route_mismatch || u.unknown_reason.is_some() {
                u.unknown_reason = Some("MANAGED_ROUTE_MISMATCH".into());
                u.issue = Some(UsageIssue::ModelBindingMismatch);
            } else {
                managed.pricing.record_cost(&mut u);
            }
        } else if let Some(price) = &self.config.capabilities.limits.price {
            if price.model != u.model {
                u.issue = Some(UsageIssue::ModelBindingMismatch);
                return Ok(u);
            }
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
    let (Some(id), Some(name)) = (id, name) else {
        return Err(invalid());
    };
    if id.is_empty() || name.is_empty() {
        return Err(invalid());
    }
    Ok(ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: arguments.unwrap_or(Value::Null),
    })
}
fn safe_retry_after(value: &str) -> Option<String> {
    (value.parse::<u64>().is_ok() || httpdate::parse_http_date(value).is_ok())
        .then(|| value.to_owned())
}
/// Retry-After as a runtime delay in milliseconds (the header is delta seconds or an
/// HTTP date). A value that overflows milliseconds is rejected, not clamped.
fn retry_delay(value: &str) -> Option<u64> {
    value
        .parse::<u64>()
        .ok()
        .and_then(|seconds| seconds.checked_mul(1000))
        .or_else(|| {
            httpdate::parse_http_date(value).ok().and_then(|date| {
                u64::try_from(
                    date.duration_since(SystemTime::now())
                        .unwrap_or_default()
                        .as_millis(),
                )
                .ok()
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
#[derive(Debug)]
pub struct ManagedModel(pub(crate) Adapter);

impl ManagedModel {
    pub fn model(&self) -> &models::ResolvedModel {
        &self.0.managed.as_ref().expect("catalog connection").model
    }
}

macro_rules! adapter {
    ($name:ident) => {
        impl ModelPort for $name {
            fn check_input_attachments(
                &self,
                attachments: &[runtrell::attachments::Attachment],
                images: &[runtrell::images::ImageReference],
            ) -> Result<()> {
                self.0.check_input_attachments(attachments, images)
            }
            fn limits(&self) -> ModelLimits {
                self.0.config.capabilities.limits.clone()
            }
            fn pricing_snapshot(&self, model: &str) -> Option<models::PricingSnapshot> {
                self.0
                    .managed
                    .as_ref()
                    .filter(|connection| connection.model.wire_model() == model)
                    .map(|connection| connection.pricing.clone())
            }
            fn compaction_input_limit(&self, _: &PreparedModelRequest) -> Option<u64> {
                self.0
                    .counts_responses_input()
                    .then_some(self.0.config.input_token_upper_bound)
            }
            fn supports_output_schema(&self) -> bool {
                self.0.config.capabilities.json_schema
            }
            fn input_upper_bound(&self, request: &PreparedModelRequest) -> Result<u64> {
                request.validate_for_send()?;
                // Size the largest feasible cap too: its decimal wire encoding
                // must fit before reservation, even when the prototype cap is 0.
                let limits = self.limits();
                let ceiling = if self.0.managed.is_some() {
                    limits.max_output
                } else {
                    limits.max_output.min(
                        limits
                            .context
                            .saturating_sub(self.0.config.input_token_upper_bound),
                    )
                };
                let body = self.0.body(
                    &self.0.config.run,
                    &request
                        .clone()
                        .with_max_output(request.max_output.max(ceiling)),
                )?;
                self.0.check_media_wire_bound(request, &body)?;
                for image in request.resolved_images() {
                    image.validate(&self.0.config.image_limits)?;
                }
                self.0.model_input_bound(&body, request)
            }
            fn retry_policy(&self) -> RetryPolicy {
                self.0.config.retry_policy.clone()
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
                self.0.lookup(context, request).await
            }
        }
    };
}

adapter!(ManagedModel);

#[cfg(test)]
macro_rules! fixture_adapter {
    ($name:ident, $feature:literal, $wire:ident) => {
        #[cfg(feature = $feature)]
        #[derive(Debug)]
        pub(crate) struct $name(pub(crate) Adapter);
        #[cfg(feature = $feature)]
        impl $name {
            pub(crate) fn new(config: Config) -> Result<Self> {
                Adapter::new(config, Wire::$wire).map(Self)
            }
        }
        #[cfg(feature = $feature)]
        adapter!($name);
    };
}
#[cfg(test)]
fixture_adapter!(OpenAi, "openai", Responses);
#[cfg(test)]
fixture_adapter!(Gemini, "gemini", GenerateContent);
#[cfg(test)]
fixture_adapter!(OpenRouter, "openrouter", Chat);

#[cfg(test)]
mod tool_name_tests;

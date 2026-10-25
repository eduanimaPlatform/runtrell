// Derived from Rig v0.45.0 (98ed06d7f1c911e6e4c7fdb5d690093bc964c514), crates/rig-core/src/error.rs.
// Copyright (c) 2024, Playgrounds Analytics Inc.
// Modified for runtrell; inherited code: MIT (LICENSES/MIT-Rig.txt); changes: Apache-2.0.
//! Serializable failures; transport-specific details are opaque host JSON.
use crate::{
    completion::UnsupportedOption,
    tool::{ToolErrorKind, ToolExecutionError},
};
use serde::{Deserialize, Serialize};
use std::{fmt, sync::Arc};
/// Normalized classification of an [`ErrorReport`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// A transport failure that produced no provider reply: a reset
    /// connection, a timeout, a protocol error, an unreadable response. It
    /// never carries a status; a reply the server made, whatever its status,
    /// is [`Self::ProviderResponse`].
    Http,
    /// JSON serialization or deserialization failed.
    Json,
    /// A URL could not be parsed.
    Url,
    /// The request could not be built.
    Request,
    /// The response could not be parsed.
    Response,
    /// The provider reported a failure without a preserved raw response.
    Provider,
    /// The provider replied and its raw response was preserved: a non-2xx
    /// status with a body, a 2xx error envelope, or a non-HTTP transport's
    /// error payload. Status, body, request id and headers are on the report.
    ProviderResponse,
    /// A tool failed; the inner kind is the tool's own classification.
    Tool(ToolErrorKind),
    /// An internal invariant was violated.
    Internal,
    /// The operation was cancelled.
    Cancelled,
    /// The operation exceeded its deadline.
    Timeout,
    /// No handler serves the requested key and effect family.
    /// The message identifies the key.
    HandlerUnavailable,
    /// Replay requested a different effect or exhausted the log.
    /// The run fails without retry rather than inventing a recorded answer.
    Divergence,
    /// Policy denied dispatch before handler execution. Not retryable under
    /// the same policy; distinct from program cancellation.
    Denied,
    /// Anything else.
    Other,
}

impl ErrorKind {
    /// Stable snake-case classification code, excluding variant payloads.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Json => "json",
            Self::Url => "url",
            Self::Request => "request",
            Self::Response => "response",
            Self::Provider => "provider",
            Self::ProviderResponse => "provider_response",
            Self::Tool(_) => "tool",
            Self::Internal => "internal",
            Self::Cancelled => "cancelled",
            Self::Timeout => "timeout",
            Self::HandlerUnavailable => "handler_unavailable",
            Self::Divergence => "divergence",
            Self::Denied => "denied",
            Self::Other => "other",
        }
    }
}

/// A serde-able error crossing a wire boundary.
///
/// Field semantics:
/// - `retryable` is the one policy signal; it is decided at conversion time
///   from the source's own classification (see [`retryable_status`]).
/// - `message` is the source's `Display`; `source_chain` is the `Display` of
///   each `source()` link, outermost first, excluding `message` itself.
/// - `code`, `http_status`, `refusal` are copied when the source had them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorReport {
    /// Normalized classification.
    pub kind: ErrorKind,
    /// Whether the same operation may reasonably be retried.
    pub retryable: bool,
    /// Human-readable description (the source's `Display`).
    pub message: String,
    /// A provider- or tool-specific machine code, when one was reported:
    /// for a provider's reply, the transport's own code when it gave one
    /// apart from the body, else the code the body names
    /// (the host adapter).
    pub code: Option<String>,
    /// The HTTP status, when the failure had one.
    pub http_status: Option<u16>,
    /// The failure was an intentional refusal rather than a fault.
    pub refusal: bool,
    /// `Display` of each `source()` link, outermost first.
    pub source_chain: Vec<String>,
    /// The provider's request id, when the failure had a response that
    /// carried one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Preserved provider failure response, including available status, body,
    /// headers, and request ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_response: Option<serde_json::Value>,
}

impl ErrorReport {
    /// Build a report of `kind` with `message` and no other metadata.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            retryable: false,
            message: message.into(),
            code: None,
            http_status: None,
            refusal: false,
            source_chain: Vec::new(),
            request_id: None,
            provider_response: None,
        }
    }

    /// Set `retryable`.
    pub fn with_retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    /// Set the machine code.
    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }

    /// Set the HTTP status.
    pub fn with_http_status(mut self, status: u16) -> Self {
        self.http_status = Some(status);
        self
    }

    /// Mark the report as an intentional refusal.
    pub fn refused(mut self) -> Self {
        self.refusal = true;
        self
    }

    /// Attach the provider's request id.
    pub fn with_request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }

    /// Whether the same operation may reasonably be retried.
    pub const fn is_retryable(&self) -> bool {
        self.retryable
    }
}

impl fmt::Display for ErrorReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for ErrorReport {}
/// Collect the `Display` of each `source()` link below `error`, outermost
/// first.
pub(crate) fn source_chain(error: &(dyn std::error::Error + 'static)) -> Vec<String> {
    let mut chain = Vec::new();
    let mut current = error.source();
    while let Some(source) = current {
        chain.push(source.to_string());
        current = source.source();
    }
    chain
}

/// A boxed request-building failure.
#[cfg(not(target_family = "wasm"))]
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// A boxed request-building failure.
#[cfg(target_family = "wasm")]
pub type BoxError = Box<dyn std::error::Error + 'static>;

/// A request-building failure shared by every clone of the error that holds
/// it.
#[cfg(not(target_family = "wasm"))]
pub type SharedError = Arc<dyn std::error::Error + Send + Sync + 'static>;

/// A request-building failure shared by every clone of the error that holds
/// it.
#[cfg(target_family = "wasm")]
pub type SharedError = Arc<dyn std::error::Error + 'static>;

/// A failed model operation. Hosts relay transport failures as a full report.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ProviderError {
    Json(Arc<serde_json::Error>),
    Request(SharedError),
    Response(String),
    Provider(String),
    Relayed(Box<ErrorReport>),
    Truncated,
    UnsupportedOption(UnsupportedOption),
}
impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(error) => write!(f, "JsonError: {error}"),
            Self::Request(error) => write!(f, "RequestError: {error}"),
            Self::Response(message) => write!(f, "ResponseError: {message}"),
            Self::Provider(message) => write!(f, "ProviderError: {message}"),
            Self::Relayed(report) => f.write_str(&report.message),
            Self::Truncated => {
                f.write_str("ResponseError: the reply ended before the provider ended it")
            }
            Self::UnsupportedOption(option) => write!(f, "RequestError: {option}"),
        }
    }
}
// The shared source itself, not its Arc: inherited downcast behavior.
impl std::error::Error for ProviderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(error) => Some(&**error),
            Self::Request(error) => Some(&**error),
            _ => None,
        }
    }
}
impl ProviderError {
    pub fn request(reason: impl Into<BoxError>) -> Self {
        Self::Request(Arc::from(reason.into()))
    }
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::Json(_) => ErrorKind::Json,
            Self::Request(_) | Self::UnsupportedOption(_) => ErrorKind::Request,
            Self::Response(_) | Self::Truncated => ErrorKind::Response,
            Self::Provider(_) => ErrorKind::Provider,
            Self::Relayed(report) => report.kind,
        }
    }
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Truncated => true,
            Self::Relayed(report) => report.retryable,
            _ => false,
        }
    }
    pub fn report(&self) -> ErrorReport {
        if let Self::Relayed(report) = self {
            return (**report).clone();
        }
        let mut report =
            ErrorReport::new(self.kind(), self.to_string()).with_retryable(self.is_retryable());
        report.source_chain = source_chain(self);
        report
    }
}
impl From<serde_json::Error> for ProviderError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(Arc::new(error))
    }
}
impl From<BoxError> for ProviderError {
    fn from(error: BoxError) -> Self {
        Self::Request(Arc::from(error))
    }
}
impl From<ProviderError> for ErrorReport {
    fn from(error: ProviderError) -> Self {
        error.report()
    }
}
impl From<&ProviderError> for ErrorReport {
    fn from(error: &ProviderError) -> Self {
        error.report()
    }
}
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct EncodeError(ProviderError);

impl EncodeError {
    /// A request that could not be built, for the given reason.
    pub fn request(reason: impl Into<BoxError>) -> Self {
        Self(ProviderError::request(reason))
    }

    /// A request that sets an option the wire or model cannot honour.
    pub fn unsupported(option: UnsupportedOption) -> Self {
        Self(ProviderError::UnsupportedOption(option))
    }

    /// The refused option, when this error is a refusal.
    pub fn unsupported_option(&self) -> Option<&UnsupportedOption> {
        match &self.0 {
            ProviderError::UnsupportedOption(option) => Some(option),
            _ => None,
        }
    }
}

impl From<EncodeError> for ProviderError {
    fn from(error: EncodeError) -> Self {
        debug_assert_eq!(error.0.kind(), ErrorKind::Request);
        error.0
    }
}

impl ToolExecutionError {
    /// The wire form of this error.
    pub fn report(&self) -> ErrorReport {
        ErrorReport::from(self)
    }
}

impl ToolExecutionError {
    /// Whether the tool may reasonably be re-run: the explicit override when
    /// one was set, else the kind's own default
    /// ([`ToolErrorKind::default_retryable`]); a kind that leaves it to the
    /// tool (`None`) is not retryable on the wire.
    pub fn is_retryable(&self) -> bool {
        self.retryable()
            .or_else(|| self.kind().default_retryable())
            .unwrap_or(false)
    }
}

impl From<&ToolExecutionError> for ErrorReport {
    fn from(error: &ToolExecutionError) -> Self {
        ErrorReport {
            kind: ErrorKind::Tool(error.kind()),
            retryable: error.is_retryable(),
            message: error.message().to_string(),
            code: error.code().map(str::to_string),
            http_status: error.http_status(),
            refusal: error.is_refusal(),
            source_chain: source_chain(error),
            request_id: None,
            provider_response: None,
        }
    }
}

impl From<ToolExecutionError> for ErrorReport {
    fn from(error: ToolExecutionError) -> Self {
        Self::from(&error)
    }
}

/// Rebuilds a tool failure from a dispatch report.
///
/// An [`ErrorKind::Tool`] report keeps its tool kind; other kinds map to the
/// closest [`ToolErrorKind`]. Retryability, code, HTTP status, and refusal are
/// copied, the message is model-visible, and the report is the `source()`.
impl From<ErrorReport> for ToolExecutionError {
    fn from(report: ErrorReport) -> Self {
        let kind = match report.kind {
            ErrorKind::Tool(kind) => kind,
            ErrorKind::Timeout => ToolErrorKind::Timeout,
            ErrorKind::Cancelled => ToolErrorKind::Cancelled,
            ErrorKind::Denied => ToolErrorKind::PermissionDenied,
            ErrorKind::HandlerUnavailable => ToolErrorKind::NotFound,
            ErrorKind::Http => ToolErrorKind::Network,
            ErrorKind::Provider | ErrorKind::ProviderResponse => ToolErrorKind::Provider,
            ErrorKind::Json
            | ErrorKind::Url
            | ErrorKind::Request
            | ErrorKind::Response
            | ErrorKind::Internal
            | ErrorKind::Divergence
            | ErrorKind::Other => ToolErrorKind::Other,
        };
        let mut error = if report.refusal {
            ToolExecutionError::refused(report.message.clone())
        } else {
            ToolExecutionError::new(kind, report.message.clone())
        }
        .with_retryable(report.retryable);
        if let Some(code) = &report.code {
            error = error.with_code(code.clone());
        }
        if let Some(status) = report.http_status {
            error = error.with_http_status(status);
        }
        error.with_source(report)
    }
}

#[cfg(test)]
mod tests;
pub const fn retryable_status(status: Option<u16>) -> bool {
    match status {
        None => false,
        Some(408 | 425 | 429) => true,
        Some(s) => s >= 500 && s <= 599,
    }
}

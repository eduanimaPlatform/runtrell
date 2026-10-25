// Derived from Rig v0.45.0 (98ed06d7f1c911e6e4c7fdb5d690093bc964c514), crates/rig-core/src/error/tests.rs.
// Copyright (c) 2024, Playgrounds Analytics Inc.
// Modified for runtrell; inherited code: MIT (LICENSES/MIT-Rig.txt); changes: Apache-2.0.
use super::*;
#[test]
fn tool_error_survives_a_report_round_trip() {
    let original = ToolExecutionError::rate_limited("slow down")
        .with_retryable(false)
        .with_code("quota")
        .with_http_status(429);
    let back = ToolExecutionError::from(original.report());
    assert_eq!(back.kind(), ToolErrorKind::RateLimited);
    assert_eq!(back.message(), "slow down");
    assert_eq!(back.model_feedback(), Some("slow down"));
    assert_eq!(back.retryable(), Some(false));
    assert_eq!(back.code(), Some("quota"));
    assert_eq!(back.http_status(), Some(429));
    assert!(!back.is_refusal());
    assert_eq!(
        back.downcast_ref::<ErrorReport>().map(|report| report.kind),
        Some(ErrorKind::Tool(ToolErrorKind::RateLimited))
    );

    let refused = ToolExecutionError::from(ToolExecutionError::refused("no").report());
    assert!(refused.is_refusal());
    assert_eq!(refused.kind(), ToolErrorKind::PermissionDenied);
}

#[test]
fn dispatch_report_kinds_map_to_tool_kinds() {
    for (kind, expected) in [
        (ErrorKind::Timeout, ToolErrorKind::Timeout),
        (ErrorKind::Cancelled, ToolErrorKind::Cancelled),
        (ErrorKind::Denied, ToolErrorKind::PermissionDenied),
        (ErrorKind::HandlerUnavailable, ToolErrorKind::NotFound),
        (ErrorKind::Http, ToolErrorKind::Network),
        (ErrorKind::ProviderResponse, ToolErrorKind::Provider),
        (ErrorKind::Internal, ToolErrorKind::Other),
    ] {
        let error = ToolExecutionError::from(ErrorReport::new(kind, "failed").with_retryable(true));
        assert_eq!(error.kind(), expected, "{kind:?}");
        assert_eq!(error.retryable(), Some(true), "{kind:?}");
    }
}

#[test]
fn source_chain_is_outermost_first() {
    #[derive(Debug, thiserror::Error)]
    #[error("inner")]
    struct Inner;
    #[derive(Debug, thiserror::Error)]
    #[error("outer")]
    struct Outer(#[source] Inner);

    let error = ToolExecutionError::new(ToolErrorKind::Other, "tool").with_source(Outer(Inner));
    let report = error.report();
    assert_eq!(report.message, "tool");
    assert_eq!(
        report.source_chain,
        vec!["outer".to_string(), "inner".to_string()]
    );
}

#[test]
fn report_round_trips_through_serde() {
    let report = ErrorReport::new(ErrorKind::Tool(ToolErrorKind::RateLimited), "slow down")
        .with_retryable(true)
        .with_code("429")
        .with_http_status(429);
    let json = serde_json::to_string(&report).expect("serialize");
    let back: ErrorReport = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, report);
    assert_eq!(report.to_string(), "slow down");
}

#[test]
fn every_error_kind_has_its_own_stable_code() {
    let kinds = [
        (ErrorKind::Http, "http"),
        (ErrorKind::Json, "json"),
        (ErrorKind::Url, "url"),
        (ErrorKind::Request, "request"),
        (ErrorKind::Response, "response"),
        (ErrorKind::Provider, "provider"),
        (ErrorKind::ProviderResponse, "provider_response"),
        (ErrorKind::Tool(ToolErrorKind::Timeout), "tool"),
        (ErrorKind::Internal, "internal"),
        (ErrorKind::Cancelled, "cancelled"),
        (ErrorKind::Timeout, "timeout"),
        (ErrorKind::HandlerUnavailable, "handler_unavailable"),
        (ErrorKind::Divergence, "divergence"),
        (ErrorKind::Denied, "denied"),
        (ErrorKind::Other, "other"),
    ];
    for (kind, code) in kinds {
        assert_eq!(kind.code(), code, "{kind:?}");
    }
}

#[test]
fn a_report_carries_its_refusal_and_request_id() {
    let report = ErrorReport::new(ErrorKind::Provider, "declined")
        .refused()
        .with_request_id("req_123");
    assert!(report.refusal);
    assert_eq!(report.request_id.as_deref(), Some("req_123"));
    assert!(!retryable_status(None));
}

#[derive(Debug, thiserror::Error)]
#[error("backend failed")]
struct NestedBackendError(#[source] std::io::Error);

#[test]
fn wrapped_document_error_retains_nested_sources() {
    let error = ProviderError::request(NestedBackendError(std::io::Error::other("document")));
    assert!(
        std::error::Error::source(&error).is_some_and(|source| source.is::<NestedBackendError>())
    );
    let report = ErrorReport::from(&error);
    assert_eq!(report.source_chain, vec!["backend failed", "document"]);
    assert_eq!(report.message, error.to_string());
}

#[test]
fn request_building_failures_share_one_shape() {
    let document = ProviderError::request(std::io::Error::other("io broke"));
    let invalid = ProviderError::request("bad name");
    assert_eq!(document.to_string(), "RequestError: io broke");
    assert_eq!(invalid.to_string(), "RequestError: bad name");
    for error in [document, invalid] {
        let report = error.report();
        assert_eq!(report.kind, ErrorKind::Request);
        assert!(!report.retryable);
        assert_eq!(report.source_chain.len(), 1, "{error}");
    }
}

#[test]
fn a_relayed_report_keeps_its_provider_response() {
    let mut report = ErrorReport::new(ErrorKind::ProviderResponse, "body")
        .with_http_status(503)
        .with_retryable(true);
    report.provider_response = Some(serde_json::json!({"status":503,"body":"body"}));
    let relayed = ProviderError::Relayed(Box::new(report.clone()));
    assert_eq!(relayed.kind(), ErrorKind::ProviderResponse);
    assert!(relayed.is_retryable());
    assert_eq!(ErrorReport::from(&relayed), report);
}

#[test]
fn a_provider_response_travels_with_the_report() {
    let mut report = ErrorReport::new(ErrorKind::ProviderResponse, "raw response");
    report.provider_response =
        Some(serde_json::json!({"body":"uninterpreted","headers":[["x",[128]]]}));
    let restored: ErrorReport =
        serde_json::from_value(serde_json::to_value(&report).unwrap()).unwrap();
    assert_eq!(restored, report);
    let error = ToolExecutionError::from(restored);
    assert_eq!(error.downcast_ref::<ErrorReport>().unwrap(), &report);
}

#[test]
fn reports_match_the_replaced_error_enums() {
    let json_error = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
    let cases = [
        (
            ProviderError::from(json_error),
            ErrorKind::Json,
            "JsonError: EOF while parsing an object at line 1 column 1",
            vec!["EOF while parsing an object at line 1 column 1"],
        ),
        (
            ProviderError::Response("bad shape".into()),
            ErrorKind::Response,
            "ResponseError: bad shape",
            vec![],
        ),
        (
            ProviderError::Provider("provider said no".into()),
            ErrorKind::Provider,
            "ProviderError: provider said no",
            vec![],
        ),
        (
            ProviderError::request(std::io::Error::other("io broke")),
            ErrorKind::Request,
            "RequestError: io broke",
            vec!["io broke"],
        ),
    ];
    for (error, kind, message, sources) in cases {
        let mut expected = ErrorReport::new(kind, message);
        expected.source_chain = sources.into_iter().map(str::to_owned).collect();
        assert_eq!(error.report(), expected);
        assert_eq!(
            serde_json::to_value(error.report()).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
    }
}

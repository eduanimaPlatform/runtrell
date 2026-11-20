// SPDX-License-Identifier: Apache-2.0
#![cfg(feature = "openai")]

use runtrell::{Error, Usage};
use runtrell_providers::models::{OperationKind, ResolveError, resolve};

#[test]
fn catalog_checks_identity_and_operation() {
    assert!(matches!(
        resolve("openai/catalog-test-model", OperationKind::Completion),
        Err(ResolveError::UnknownModel)
    ));
    assert!(matches!(
        resolve("openai/gpt-6.2-sol", OperationKind::RasterEdit),
        Err(ResolveError::UnsupportedOperation)
    ));
}

#[test]
fn prices_the_whole_request_at_the_measured_input_band() {
    let model = resolve("openai/gpt-6.2-sol", OperationKind::Completion).unwrap();
    let mut usage = Usage {
        model: "gpt-6.2-sol".into(),
        input: Some(272_000),
        output: Some(50),
        cached: Some(20),
        reasoning: Some(10),
        ..Usage::default()
    };
    assert_eq!(model.cost(&usage), Err(Error::UsageUnknown));
    usage.quantities.insert("cache_write_tokens".into(), 0);
    assert_eq!(model.cost(&usage).unwrap().micro_usd, 544_462);
    usage.input = Some(272_001);
    assert_eq!(model.cost(&usage).unwrap().micro_usd, 1_088_678);
}

#[test]
fn direct_svg_is_rejected_before_credentials() {
    use runtrell_providers::{ConnectError, Connection, OpenAiImages};
    use std::sync::Arc;

    let err = OpenAiImages::connect(
        "openai/gpt-image-2.6-sunburst",
        OperationKind::SvgGenerate,
        None,
        Connection {
            run: runtrell::RunHandle {
                tenant: "tenant".into(),
                id: "run".into(),
            },
            session: None,
            credentials: Arc::new(|_| panic!("SVG resolved a credential")),
            attachment_resolver: None,
            retry_policy: Default::default(),
            timeout_ms: 1_000,
            context_cap: None,
            input_token_cap: None,
            max_output_tokens: None,
            reasoning: None,
        },
    )
    .unwrap_err();
    assert!(
        matches!(err, ConnectError::Configuration(Error::Unsupported(reason)) if reason.contains("max_output_tokens"))
    );
}

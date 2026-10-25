// Derived from Rig v0.45.0 (98ed06d7f1c911e6e4c7fdb5d690093bc964c514), crates/rig-core/src/completion/request/response_identity_tests.rs.
// Copyright (c) 2024, Playgrounds Analytics Inc.
// Modified for runtrell; inherited code: MIT (LICENSES/MIT-Rig.txt); changes: Apache-2.0.
use super::*;

/// `provider_request_id` is skipped when `None`, so a response written
/// without one loads with the field `None` (rig#2265).
#[test]
fn completion_response_without_request_id_deserializes() {
    let response: CompletionResponse = serde_json::from_str(
        r#"{"choice": [{"type": "text", "text": "hi"}],
                "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2,
                          "cached_input_tokens": 0, "cache_creation_input_tokens": 0,
                          "reasoning_tokens": 0},
                "origin": {"api": "test.api", "provider": "test", "model": ""},
                "raw": null}"#,
    )
    .expect("a CompletionResponse without a request id should load");
    assert_eq!(response.provider_request_id, None);
    assert_eq!(response.identity(), ResponseIdentity::default());
}

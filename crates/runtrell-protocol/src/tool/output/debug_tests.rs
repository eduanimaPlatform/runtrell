// Derived from Rig v0.45.0 (98ed06d7f1c911e6e4c7fdb5d690093bc964c514), crates/rig-core/src/tool/output/debug_tests.rs.
// Copyright (c) 2024, Playgrounds Analytics Inc.
// Modified for runtrell; inherited code: MIT (LICENSES/MIT-Rig.txt); changes: Apache-2.0.
use crate::message::ImageMediaType;

use super::*;

#[test]
fn debug_reports_shape_without_tool_content() {
    let output = ToolOutput::content(vec![
        ToolResultContent::text("Bearer secret-tool-output"),
        ToolResultContent::json(serde_json::json!({
            "credential": "secret-json-output"
        })),
        ToolResultContent::image_base64("secret-image-output", Some(ImageMediaType::PNG), None),
    ])
    .expect("fixture content is non-empty");

    let debug = format!("{output:?}");
    assert!(debug.contains("content_count: 3"));
    assert!(debug.contains("text"));
    assert!(debug.contains("json"));
    assert!(debug.contains("image"));
    for secret in [
        "secret-tool-output",
        "secret-json-output",
        "secret-image-output",
    ] {
        assert!(!debug.contains(secret));
    }
}

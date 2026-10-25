// SPDX-License-Identifier: Apache-2.0
use runtrell::*;
use serde_json::json;
#[test]
fn schema_contract() {
    let schema = json!({"$defs":{"positive":{"type":"integer","minimum":1}},"type":"object","properties":{"count":{"$ref":"#/$defs/positive"}},"required":["count"]});
    assert!(validate_schema(&json!({"count":2}), &schema).is_ok());
    let errors = schema_diagnostics(&json!({"count":0}), &schema).unwrap();
    assert_eq!(errors[0].path, "/count");
    assert!(errors[0].schema_path.as_ref().unwrap().contains("minimum"));
    assert!(validate_schema(&json!("bad"), &json!({"format":"email"})).is_err());
    for schema in [
        json!({"typo":true}),
        json!({"$ref":"https://example.org/schema"}),
        json!({"$schema":"http://json-schema.org/draft-07/schema#"}),
        json!({"format":"unknown-format"}),
        json!({"$defs":{"unused":{"format":"unknown-format"}}}),
    ] {
        assert!(validate_schema(&json!(null), &schema).is_err());
    }
    assert!(validate_schema(&json!({"minimum":42}), &json!({"const":{"minimum":42}})).is_ok());
    assert!(validate_schema(&json!(null), &json!(false)).is_err());
}

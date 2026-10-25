// SPDX-License-Identifier: Apache-2.0
use runtrell::*;
use serde_json::json;
#[test]
fn json_syntax_positions_cover_multibyte_eof_and_trailing_input() {
    for (bytes, line, column, offset) in [
        ("{\n  \"answer\": }", 2, 13, 14),
        ("{\"é\": }", 1, 8, 7),
        ("{\n\"x\":", 2, 4, 6),
        ("{} x", 1, 4, 3),
        ("", 1, 0, 0),
    ] {
        let diagnostic = schema::parse_json(bytes.as_bytes()).unwrap_err().remove(0);
        assert_eq!(diagnostic.code, "JSON_SYNTAX");
        assert_eq!(diagnostic.path, "");
        assert_eq!(
            diagnostic.position,
            Some(JsonPosition {
                line,
                column,
                byte_offset: offset
            })
        );
        assert!(!diagnostic.cause.is_empty());
        assert!(!diagnostic.hint.is_empty());
    }
    assert_eq!(
        schema::parse_json(b"\"valid string\"").unwrap(),
        json!("valid string")
    );
    assert_eq!(schema::parse_json(b"null").unwrap(), json!(null));
}
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

#[test]
fn checkpoint_json_hash_ignores_object_key_order() {
    let first = serde_json::from_str(r#"{"z":0,"a":{"y":1,"b":2}}"#).unwrap();
    let reopened = serde_json::from_str(r#"{"a":{"b":2,"y":1},"z":0}"#).unwrap();
    assert_eq!(digest(&first), digest(&reopened));
    assert_ne!(digest(&json!([1, 2])), digest(&json!([2, 1])));
}

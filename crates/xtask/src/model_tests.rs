// SPDX-License-Identifier: Apache-2.0
use runtrell::models::{OperationKind, Unit, generate};
use serde_json::{Value, json};

const SCHEMA: &str = include_str!("../../../catalog/models.schema.json");
const SOURCE: &str = include_str!("../../../catalog/models.json");
const EXAMPLE: &str = include_str!("../../runtrell/src/models/fixtures/pricing-example.json");

#[test]
fn approved_shapes_and_canonical_artifacts() {
    generate(EXAMPLE, SCHEMA, "2026-11-20").unwrap();
    let (index, listing) = generate(SOURCE, SCHEMA, "2026-11-20").unwrap();
    assert_eq!(index, include_str!("../../runtrell/src/models/index.json"));
    assert_eq!(listing, include_str!("../../../catalog/README.md"));
}

#[test]
fn reject_nested_duplicate_keys_and_embedding_disguises() {
    let original: Value = serde_json::from_str(SOURCE).unwrap();
    for (path, value) in [
        ("/operations/gpt-6.2-sol/kind", json!("embedding")),
        ("/operations/gpt-6.2-sol/output", json!(["vector"])),
        (
            "/operations/gpt-6.2-sol/bindings/standard/request_model",
            json!("text-embedding-3-large"),
        ),
        ("/profiles/openai-responses/path", json!("/v1/embeddings")),
        (
            "/operations/gpt-6.2-sol/bindings/standard/usage_rule",
            json!("openai.unregistered@v1"),
        ),
        ("/operations/gpt-6.2-sol/bindings/standard", {
            let mut binding = original
                .pointer("/operations/gpt-6.2-sol/bindings/standard")
                .unwrap()
                .clone();
            binding["response_aliases"] = json!(["text-embedding-3-large"]);
            binding
        }),
        (
            "/price_profiles/gpt-6.2-sol/components/input/rates/1/when",
            json!({"prompt_tokens":{"min":272000}}),
        ),
        (
            "/price_profiles/gpt-6.2-sol/components/input/rates/1/when",
            json!({"prompt_tokens":{"min":272002}}),
        ),
        (
            "/price_profiles/gpt-6.2-sol/components/input/rates/0/per",
            json!("0"),
        ),
        (
            "/evidence/openai-responses/url",
            json!("https://developers.openai.com@localhost/private"),
        ),
        (
            "/evidence/openai-responses/url",
            json!("https://developers.openai.com/a?token=example"),
        ),
        ("/evidence/openai-responses/checked_on", json!("2099-01-01")),
    ] {
        let mut invalid = original.clone();
        *invalid.pointer_mut(path).unwrap() = value;
        assert!(
            generate(&invalid.to_string(), SCHEMA, "2026-11-20").is_err(),
            "{path}"
        );
    }
    let duplicate = SOURCE.replacen(
        "\"method\": \"POST\"",
        "\"method\": \"GET\", \"method\": \"POST\"",
        1,
    );
    assert!(
        generate(&duplicate, SCHEMA, "2026-11-20")
            .unwrap_err()
            .to_string()
            .contains("duplicate key")
    );
    assert!(serde_json::from_str::<OperationKind>("\"embed_batch\"").is_err());
    assert!(serde_json::from_str::<Unit>("\"vector\"").is_err());
}

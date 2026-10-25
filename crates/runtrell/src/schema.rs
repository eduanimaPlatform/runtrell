// SPDX-License-Identifier: Apache-2.0
//! Offline Draft 2020-12 validation, shared by tools and trusted validators.
use crate::*;
use serde_json::Value;

fn inspect(schema: &Value) -> Result<()> {
    let Some(obj) = schema.as_object() else {
        return if schema.is_boolean() {
            Ok(())
        } else {
            Err(Error::Invalid("SCHEMA_INVALID".into()))
        };
    };
    for (key, value) in obj {
        match key.as_str() {
            "format" => {
                jsonschema::draft202012::options()
                    .should_validate_formats(true)
                    .should_ignore_unknown_formats(false)
                    .build(&serde_json::json!({"format":value}))
                    .map_err(|e| Error::Invalid(format!("SCHEMA_INVALID: {e}")))?;
            }
            "$schema" if value != "https://json-schema.org/draft/2020-12/schema" => {
                return Err(Error::Unsupported("schema dialect".into()));
            }
            "$ref" | "$dynamicRef" if !value.as_str().is_some_and(|s| s.starts_with('#')) => {
                return Err(Error::Unsupported("external schema reference".into()));
            }
            "$id" | "$vocabulary" | "contentSchema" | "contentEncoding" | "contentMediaType" => {
                return Err(Error::Unsupported(format!("schema keyword {key}")));
            }
            "$defs" | "properties" | "patternProperties" | "dependentSchemas" => {
                for child in value
                    .as_object()
                    .ok_or_else(|| Error::Invalid("schema map".into()))?
                    .values()
                {
                    inspect(child)?;
                }
            }
            "allOf" | "anyOf" | "oneOf" | "prefixItems" => {
                for child in value
                    .as_array()
                    .ok_or_else(|| Error::Invalid("schema array".into()))?
                {
                    inspect(child)?;
                }
            }
            "items"
            | "contains"
            | "not"
            | "if"
            | "then"
            | "else"
            | "additionalProperties"
            | "unevaluatedProperties"
            | "unevaluatedItems"
            | "propertyNames" => inspect(value)?,
            "$schema" | "$ref" | "$dynamicRef" | "$anchor" | "$dynamicAnchor" | "$comment"
            | "type" | "enum" | "const" | "required" | "dependentRequired" | "minProperties"
            | "maxProperties" | "minItems" | "maxItems" | "uniqueItems" | "minContains"
            | "maxContains" | "minimum" | "maximum" | "exclusiveMinimum" | "exclusiveMaximum"
            | "multipleOf" | "minLength" | "maxLength" | "pattern" | "title" | "description"
            | "default" | "examples" | "deprecated" | "readOnly" | "writeOnly" => {}
            _ => return Err(Error::Unsupported(format!("schema keyword {key}"))),
        }
    }
    Ok(())
}
pub fn compile_schema(schema: &Value) -> Result<jsonschema::Validator> {
    inspect(schema)?;
    jsonschema::draft202012::options()
        .should_validate_formats(true)
        .should_ignore_unknown_formats(false)
        .build(schema)
        .map_err(|e| Error::Invalid(format!("SCHEMA_INVALID: {e}")))
}
/// `path` is the instance pointer; `schema_path` is the schema pointer.
pub fn schema_diagnostics(value: &Value, schema: &Value) -> Result<Vec<Diagnostic>> {
    let validator = compile_schema(schema)?;
    Ok(validator
        .iter_errors(value)
        .map(|e| Diagnostic {
            code: "SCHEMA_VALIDATION".into(),
            severity: "error".into(),
            path: e.instance_path().to_string(),
            schema_path: Some(e.schema_path().to_string()),
            field: None,
            artifact: None,
            file: None,
            span: None,
            cause: e.to_string(),
            hint: "Correct the candidate at the instance pointer".into(),
        })
        .collect())
}
pub fn validate_schema(value: &Value, schema: &Value) -> Result<()> {
    let diagnostics = schema_diagnostics(value, schema)?;
    if diagnostics.is_empty() {
        Ok(())
    } else {
        Err(Error::Invalid(
            serde_json::to_string(&diagnostics).map_err(|_| Error::Storage)?,
        ))
    }
}
/// The same evaluator can issue persisted evidence for structured/artifact targets.
pub struct SchemaValidator {
    pub key: data::ValidatorKey,
    pub schema: Value,
}
impl data::TargetValidator for SchemaValidator {
    fn key(&self) -> data::ValidatorKey {
        self.key.clone()
    }
    fn check(&self, target: &data::Target) -> Result<Vec<Diagnostic>> {
        schema_diagnostics(&target.value, &self.schema)
    }
}

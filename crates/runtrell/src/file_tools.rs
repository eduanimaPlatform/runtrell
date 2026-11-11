// SPDX-License-Identifier: Apache-2.0
//! Standard tools are ordinary Tool adapters; Runtime is the invocation authority.
use crate::{data::*, *};
use serde_json::{Value, json};

fn model_text_range(range: ByteRange) -> Result<Value> {
    let text =
        String::from_utf8(range.bytes).map_err(|_| Error::Invalid("NON_UTF8_TEXT".into()))?;
    if crate::data::binary_text(&text) {
        return Err(Error::Invalid("BINARY_TEXT".into()));
    }
    Ok(json!({
        "text": text,
        "offset": range.offset,
        "total": range.total,
        "truncated": range.truncated,
    }))
}

fn text_media_type(media_type: &str) -> bool {
    let essence = media_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    essence.starts_with("text/")
        || essence == "application/json"
        || essence == "application/xml"
        || (essence.starts_with("application/")
            && (essence.ends_with("+json") || essence.ends_with("+xml")))
}

fn model_stored_output(stored: StoredOutput) -> Result<Value> {
    Ok(match stored {
        StoredOutput::Inline(value) => json!({"Inline": value}),
        StoredOutput::External { artifact, preview } => {
            let valid_len = match std::str::from_utf8(&preview.bytes) {
                Ok(_) => preview.bytes.len(),
                Err(error) if error.error_len().is_none() => error.valid_up_to(),
                Err(_) => return Err(Error::Storage),
            };
            let text =
                std::str::from_utf8(&preview.bytes[..valid_len]).map_err(|_| Error::Storage)?;
            json!({"External": {
                "artifact": artifact,
                "preview": {
                    "text": text,
                    "offset": preview.offset,
                    "total": preview.total,
                    "truncated": preview.truncated || valid_len < preview.bytes.len(),
                }
            }})
        }
    })
}
#[derive(Clone, Copy, Debug)]
pub enum FileAction {
    List,
    Read,
    Write,
    Delete,
    Search,
    ExactReplace,
    ArtifactRead,
}
#[derive(Clone)]
pub struct FileTool<S = DataStore> {
    pub store: S,
    pub access: Access,
    pub action: FileAction,
}
enum FileToolOutcome {
    Value(Value),
    BinaryArtifact(ArtifactRef),
}
impl<S: StandardToolStore> FileTool<S> {
    /// Explicit allowlist: unknown errors and resource stop conditions are fatal.
    fn repair_failure<'a>(&self, error: &'a Error) -> Option<(&'a str, String)> {
        let policy = self.store.policy();
        match error {
            Error::Invalid(code) => {
                let range_hint = match code.as_str() {
                    "PAGE_LIMIT" => Some(format!("Use limit in 1..={}.", policy.max_entries)),
                    "RANGE_LIMIT" => Some(format!("Use length in 1..={} bytes.", policy.max_read)),
                    "SEARCH_LIMIT" => Some(format!("Use query of 1..={MAX_SEARCH_QUERY_BYTES} UTF-8 bytes, limit in 1..={}, and max_bytes in {MIN_SEARCH_BYTES}..={}.", policy.max_entries, policy.max_read)),
                    "EDIT_LIMIT" => Some(format!("Use 1..={} edits with combined old_text and new_text at most {} UTF-8 bytes.", policy.max_edits, policy.max_edit_bytes)),
                    "PATH_NOT_CANONICAL" => Some(format!("Use a relative ASCII path of 1..={MAX_LOGICAL_PATH_BYTES} bytes with components of 1..={MAX_PATH_COMPONENT_BYTES} bytes, without empty, dot or parent components.")),
                    _ => None,
                };
                if let Some(hint) = range_hint {
                    return Some((code, hint));
                }
                let hint = match code.as_str() {
                    "NO_MATCH" => "Read the current file and use exact existing text as old_text.",
                    "MULTIPLE_MATCHES" => {
                        "Include more surrounding context so old_text matches exactly once."
                    }
                    "EMPTY_MATCH" => "Use nonempty old_text copied from the current file.",
                    "PATH_ALIAS" | "FILE_PARENT" | "DUPLICATE_PATH" => {
                        "List files and choose distinct paths without case aliases or file-parent collisions."
                    }
                    "CURSOR" => "Restart listing or search without a cursor.",
                    "RANGE" => "Read within the current file size using a valid offset.",
                    "BINARY" | "NON_UTF8" => {
                        "Exact replacement requires UTF-8 text without binary control characters; choose a text file and text edits."
                    }
                    "NON_UTF8_TEXT" => {
                        "Choose a UTF-8 text source or request a range aligned to UTF-8 character boundaries."
                    }
                    "BINARY_TEXT" => {
                        "The selected text contains binary control characters. Choose a plain text source."
                    }
                    "expected_revision" | "expected_revision required" => {
                        "Use the current integer revision, or explicit null only when creating a file."
                    }
                    _ => return None,
                };
                Some((code, hint.into()))
            }
            // Artifact conflicts can mean integrity/authority failures, not CAS.
            Error::Conflict if !matches!(self.action, FileAction::ArtifactRead) => Some((
                "CONFLICT",
                "List/read the current workspace and refresh expected_revision, expected_digest or cursor before a new call.".into(),
            )),
            Error::NotFound if !matches!(self.action, FileAction::ArtifactRead) => Some((
                "NOT_FOUND",
                "List the workspace and use an existing path; use files.write with explicit null to create a new file.".into(),
            )),
            _ => None,
        }
    }
    fn name(&self) -> &'static str {
        match self.action {
            FileAction::List => "files.list",
            FileAction::Read => "files.read",
            FileAction::Write => "files.write",
            FileAction::Delete => "files.delete",
            FileAction::Search => "files.search",
            FileAction::ExactReplace => "files.exact_replace",
            FileAction::ArtifactRead => "artifacts.read",
        }
    }
    fn schema(&self) -> Value {
        let policy = self.store.policy();
        let text = json!({"type":"string"});
        let integer = json!({"type":"integer","minimum":0,"maximum":u64::MAX});
        let path = json!({"type":"string","minLength":1,"maxLength":MAX_LOGICAL_PATH_BYTES,"pattern":format!(r"^(?!(?:\.|\.\.)(?:/|$))[A-Za-z0-9._-]{{1,{MAX_PATH_COMPONENT_BYTES}}}(?:/(?!(?:\.|\.\.)(?:/|$))[A-Za-z0-9._-]{{1,{MAX_PATH_COMPONENT_BYTES}}})*$")});
        let page = json!({"type":"integer","minimum":1,"maximum":policy.max_entries});
        let length = json!({"type":"integer","minimum":1,"maximum":policy.max_read});
        let edit_text = json!({"type":"string","maxLength":policy.max_edit_bytes,"description":format!("Combined old_text/new_text across all edits must be at most {} UTF-8 bytes; maxLength counts Unicode characters.", policy.max_edit_bytes)});
        let mut old_text = edit_text.clone();
        old_text["minLength"] = json!(1);
        let mut write_text = json!({"type":"string","description":"Workspace capacity is measured in UTF-8 bytes across all files; maxLength counts Unicode characters."});
        if let Some(class) = policy.classes.get(&DataClass::Workspace) {
            write_text["maxLength"] = json!(class.max_bytes);
        }
        let (properties, required) = match self.action {
            FileAction::List => (json!({"limit":page,"cursor":text}), vec!["limit"]),
            FileAction::Read => (
                json!({"path":path,"revision":integer,"offset":integer,"length":length}),
                vec!["path", "revision", "offset", "length"],
            ),
            FileAction::Write => (
                json!({"path":path,"expected_revision":{"type":["integer","null"],"minimum":0,"maximum":u64::MAX,"description":"Current integer revision; explicit null to create a new file"},"text":write_text}),
                vec!["path", "expected_revision", "text"],
            ),
            FileAction::Delete => (
                json!({"path":path,"expected_revision":integer}),
                vec!["path", "expected_revision"],
            ),
            FileAction::Search => (
                json!({"query":{"type":"string","minLength":1,"maxLength":MAX_SEARCH_QUERY_BYTES,"description":format!("Nonempty query, at most {MAX_SEARCH_QUERY_BYTES} UTF-8 bytes; maxLength counts Unicode characters.")},"cursor":text,"limit":page,"max_bytes":{"type":"integer","minimum":MIN_SEARCH_BYTES,"maximum":policy.max_read}}),
                vec!["query", "limit", "max_bytes"],
            ),
            FileAction::ExactReplace => (
                json!({"expected_revision":integer,"edits":{"type":"array","minItems":1,"maxItems":policy.max_edits,"items":{"type":"object","properties":{"path":path,"expected_digest":text,"old_text":old_text,"new_text":edit_text},"required":["path","expected_digest","old_text","new_text"],"additionalProperties":false}}}),
                vec!["expected_revision", "edits"],
            ),
            FileAction::ArtifactRead => (
                json!({"artifact":{"type":"object","description":"Exact saved ArtifactRef returned by an earlier tool; never construct or alter ownership, metadata, digest or manifest."},"offset":integer,"length":length}),
                vec!["artifact", "offset", "length"],
            ),
        };
        json!({"type":"object","properties":properties,"required":required,"additionalProperties":false,"description":format!("Serialized JSON input must not exceed {} bytes. Storage capacity, ownership and integrity checks still apply at execution.", policy.max_edit_bytes)})
    }
    fn bound(&self, context: &ToolContext) -> Result<()> {
        if context.claim.run != self.access.actor().owner
            || context.permission != self.descriptor().permission
            || context.account_binding != self.descriptor().account
        {
            return Err(Error::PermissionDenied);
        }
        Ok(())
    }
    async fn execute(&self, input: Value) -> Result<FileToolOutcome> {
        let string = |name: &str| {
            input[name]
                .as_str()
                .ok_or_else(|| Error::Invalid(format!("missing {name}")))
        };
        let number = |name: &str| {
            input[name]
                .as_u64()
                .ok_or_else(|| Error::Invalid(format!("missing {name}")))
        };
        let size =
            |name: &str| usize::try_from(number(name)?).map_err(|_| Error::Invalid("size".into()));
        let cursor = || {
            input
                .get("cursor")
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        match self.action {
            FileAction::List => Ok(FileToolOutcome::Value(json!(
                self.store
                    .list(&self.access, cursor(), size("limit")?)
                    .await?
            ))),
            FileAction::Read => Ok(FileToolOutcome::Value(model_text_range(
                self.store
                    .read(
                        &self.access,
                        string("path")?,
                        number("revision")?,
                        number("offset")?,
                        size("length")?,
                    )
                    .await?,
            )?)),
            FileAction::Write => {
                let expected = match input.get("expected_revision") {
                    Some(Value::Null) => None,
                    Some(v) => Some(
                        v.as_u64()
                            .ok_or_else(|| Error::Invalid("expected_revision".into()))?,
                    ),
                    None => return Err(Error::Invalid("expected_revision required".into())),
                };
                Ok(FileToolOutcome::Value(json!(
                    self.store
                        .write(
                            &self.access,
                            string("path")?,
                            expected,
                            string("text")?.as_bytes()
                        )
                        .await?
                )))
            }
            FileAction::Delete => {
                self.store
                    .delete(&self.access, string("path")?, number("expected_revision")?)
                    .await?;
                Ok(FileToolOutcome::Value(json!({"saved":true})))
            }
            FileAction::Search => Ok(FileToolOutcome::Value(json!(
                self.store
                    .search(
                        &self.access,
                        string("query")?,
                        cursor(),
                        size("limit")?,
                        size("max_bytes")?
                    )
                    .await?
            ))),
            FileAction::ExactReplace => {
                let edits = serde_json::from_value(input["edits"].clone())
                    .map_err(|e| Error::Invalid(e.to_string()))?;
                Ok(FileToolOutcome::Value(json!(
                    self.store
                        .exact_replace(&self.access, number("expected_revision")?, edits)
                        .await?
                )))
            }
            FileAction::ArtifactRead => {
                let reference = serde_json::from_value(input["artifact"].clone())
                    .map_err(|e| Error::Invalid(e.to_string()))?;
                let range = self
                    .store
                    .read_artifact(&self.access, &reference, number("offset")?, size("length")?)
                    .await?;
                if !text_media_type(&reference.media_type) {
                    return Ok(FileToolOutcome::BinaryArtifact(reference));
                }
                Ok(FileToolOutcome::Value(model_text_range(range)?))
            }
        }
    }
}
impl<S: StandardToolStore> Tool for FileTool<S> {
    fn descriptor(&self) -> ToolDescriptor {
        let write = matches!(
            self.action,
            FileAction::Write | FileAction::Delete | FileAction::ExactReplace
        );
        ToolDescriptor {
            name: self.name().into(),
            version: "runtrell-files-4".into(),
            account: "local-files".into(),
            permission: if write {
                "workspace.write"
            } else {
                "workspace.read"
            }
            .into(),
            capability: "logical-workspace".into(),
            input_schema: self.schema(),
            output_schema: json!({"type":"object"}),
            side_effect: write,
            timeout: 30_000,
            max_output_bytes: self
                .store
                .policy()
                .max_read
                .saturating_mul(6)
                .saturating_add(4096),
            reserve: Amount::default(),
            quantities: Default::default(),
            conflict_scope: Some(format!(
                "workspace:{}:{}:{}",
                self.access.namespace().owner.tenant,
                self.access.namespace().owner.id,
                self.access.namespace().agent
            )),
            retry: RetryPolicy {
                max_attempts: 1,
                backoff: 0,
                max_elapsed: 30_000,
            },
        }
    }
    async fn invoke(&self, context: &ToolContext, input: Value) -> Result<ExternalResult> {
        self.bound(context)?;
        if serde_json::to_vec(&input)
            .map_err(|_| Error::Storage)?
            .len()
            > self.store.policy().max_edit_bytes
        {
            return Ok(ExternalResult::Failed {
                repair_hint: None,
                physical_evidence: None,
                code: "INPUT_LIMIT".into(),
                usage: Usage::measured(0, 0, 0),
                retry_after: None,
                provider_diagnostic: None,
            });
        }
        Ok(match self.execute(input).await {
            Ok(FileToolOutcome::Value(output)) => ExternalResult::Applied {
                physical_evidence: None,
                output,
                usage: Usage::measured(0, 0, 0),
                external_id: None,
            },
            Ok(FileToolOutcome::BinaryArtifact(reference)) => ExternalResult::Failed {
                repair_hint: Some(format!(
                    "Binary artifact: media_type={}, size={}, sha256={}. Use document.view for documents or an image input for images.",
                    reference.media_type, reference.bytes, reference.hash
                )),
                physical_evidence: None,
                code: "BINARY_ARTIFACT".into(),
                usage: Usage::measured(0, 0, 0),
                retry_after: None,
                provider_diagnostic: None,
            },
            Err(e) => ExternalResult::Failed {
                repair_hint: self.repair_failure(&e).map(|(_, hint)| hint),
                physical_evidence: None,
                code: self
                    .repair_failure(&e)
                    .map_or_else(|| e.to_string(), |(code, _)| code.into()),
                usage: Usage::measured(0, 0, 0),
                retry_after: None,
                provider_diagnostic: None,
            },
        })
    }
    // A write followed by a crash before ledger recording is ambiguous; never replay it.
    async fn lookup(&self, context: &ToolContext, _input: Value) -> Result<Lookup> {
        self.bound(context)?;
        Ok(Lookup::Unknown)
    }
}
/// Fingerprints include schemas, implementation version, account, scope and limits.
pub fn catalog_digest(descriptor: &ToolDescriptor) -> String {
    digest(&json!(descriptor))
}
pub fn pin_catalog<T: Tool>(
    run: &RunRecord,
    policy: &impl Policy,
    tools: &[T],
) -> Result<Vec<ToolDescriptor>> {
    let mut names = std::collections::BTreeSet::new();
    let mut result = Vec::new();
    for tool in tools {
        let d = tool.descriptor();
        crate::schema::compile_schema(&d.input_schema)?;
        crate::schema::compile_schema(&d.output_schema)?;
        if !names.insert(d.name.clone()) {
            return Err(Error::Conflict);
        }
        if policy.authorize(run, &d.name, &d.permission).is_ok() {
            result.push(d);
        }
    }
    Ok(result)
}
/// Opt-in envelope for tools producing large output. The model receives only the
/// bounded preview plus an owned Artifact reference; fetching uses ArtifactRead.
pub struct StoredOutputTool<T, S = DataStore> {
    pub inner: T,
    pub store: S,
    pub access: Access,
    pub inline_limit: usize,
    pub now: u64,
}
impl<T: Tool, S: StandardToolStore> StoredOutputTool<T, S> {
    async fn wrap(&self, context: &ToolContext, result: ExternalResult) -> Result<ExternalResult> {
        if context.claim.run != self.access.actor().owner {
            return Err(Error::PermissionDenied);
        }
        Ok(match result {
            ExternalResult::Applied {
                physical_evidence,
                output,
                usage,
                external_id,
            } => {
                validate_schema(&output, &self.inner.descriptor().output_schema)?;
                let stored = self
                    .store
                    .store_output(
                        &self.access,
                        &context.operation,
                        output,
                        self.inline_limit,
                        self.now,
                    )
                    .await?;
                ExternalResult::Applied {
                    physical_evidence,
                    output: model_stored_output(stored)?,
                    usage,
                    external_id,
                }
            }
            other => other,
        })
    }
}
impl<T: Tool, S: StandardToolStore> Tool for StoredOutputTool<T, S> {
    async fn authorize_input(&self, run: &RunRecord, input: &Value) -> Result<()> {
        self.inner.authorize_input(run, input).await
    }
    async fn authorize_result(&self, run: &RunRecord, output: &Value) -> Result<()> {
        self.inner.authorize_result(run, output).await
    }
    async fn quote(&self, run: &RunRecord, input: &Value) -> Result<Amount> {
        self.inner.quote(run, input).await
    }
    fn operation_kind(&self) -> OperationKind {
        self.inner.operation_kind()
    }
    fn requires_recovery_preparation(&self) -> bool {
        self.inner.requires_recovery_preparation()
    }
    async fn prepare_recovery(
        &self,
        context: &PreparationContext,
        input: &Value,
    ) -> Result<Option<RecoveryProof>> {
        self.inner.prepare_recovery(context, input).await
    }

    fn descriptor(&self) -> ToolDescriptor {
        let mut d = self.inner.descriptor();
        d.version = format!("{}:stored-output-2:{}", d.version, self.inline_limit);
        d.output_schema = json!({"type":"object"});
        d.max_output_bytes = self.inline_limit.saturating_mul(6).saturating_add(8192);
        d
    }
    async fn invoke(&self, context: &ToolContext, input: Value) -> Result<ExternalResult> {
        self.wrap(context, self.inner.invoke(context, input).await?)
            .await
    }
    async fn lookup(&self, context: &ToolContext, input: Value) -> Result<Lookup> {
        Ok(match self.inner.lookup(context, input).await? {
            Lookup::Found(result) => Lookup::Found(self.wrap(context, result).await?),
            other => other,
        })
    }
    async fn cancel(&self, context: &ToolContext, input: Value) -> Result<Progress> {
        self.inner.cancel(context, input).await
    }
}
/// Validation is available through the same ordinary Tool interface as files.
pub struct ValidationTool<V, S = DataStore> {
    pub adapter: std::sync::Arc<DataValidator<V, S>>,
    pub run: RunRecord,
    /// Host supplied operation deadline in milliseconds; must be positive.
    pub timeout: u64,
}
impl<V: TargetValidator + 'static, S: EvidenceStore + StandardToolStore + 'static> Tool
    for ValidationTool<V, S>
{
    fn descriptor(&self) -> ToolDescriptor {
        let key = self.adapter.validator.key();
        ToolDescriptor {
            name: "validate".into(),
            version: format!("{}:{}", key.name, key.version),
            account: "local-validation".into(),
            permission: "validate".into(),
            capability: "trusted-validator".into(),
            input_schema: json!({"type":"object"}),
            output_schema: json!({"type":"object"}),
            side_effect: false,
            timeout: self.timeout,
            max_output_bytes: self.adapter.store.policy().max_read,
            reserve: Amount::default(),
            quantities: Default::default(),
            conflict_scope: Some("validation-evidence".into()),
            retry: RetryPolicy {
                max_attempts: 1,
                backoff: 0,
                max_elapsed: self.timeout,
            },
        }
    }
    async fn invoke(&self, context: &ToolContext, input: Value) -> Result<ExternalResult> {
        if context.claim.run != self.run.handle
            || context.permission != "validate"
            || context.account_binding != "local-validation"
        {
            return Err(Error::PermissionDenied);
        }
        let adapter = self.adapter.clone();
        let run = self.run.clone();
        let receipt = tokio::task::spawn_blocking(move || adapter.validate(&run, &input))
            .await
            .map_err(|_| Error::Storage)??;
        Ok(ExternalResult::Applied {
            physical_evidence: None,
            output: json!(receipt),
            usage: Usage::measured(0, 0, 0),
            external_id: None,
        })
    }
    async fn lookup(&self, context: &ToolContext, _: Value) -> Result<Lookup> {
        if context.claim.run != self.run.handle
            || !matches!(context.permission.as_str(), "validate" | "reconcile")
            || context.account_binding != "local-validation"
            || self.adapter.access.actor().owner != self.run.handle
        {
            return Err(Error::PermissionDenied);
        }
        // EvidenceStore records target/validator evidence, not the originating
        // operation. Matching evidence can predate this invocation, so it cannot
        // establish the outcome of a timed-out operation.
        Ok(Lookup::Unknown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stored_output_preview_trims_an_incomplete_utf8_character() {
        let store = DataStore::memory(StoragePolicy::default());
        let access = Access::owner(Namespace {
            owner: RunHandle {
                tenant: "tenant".into(),
                id: "run".into(),
            },
            agent: "main".into(),
        });
        let value = json!({"text":"é".repeat(10)});
        let serialized = serde_json::to_vec(&value).unwrap();
        let split = serialized.iter().position(|b| *b == 0xc3).unwrap() + 1;
        let stored = store
            .store_output(&access, "unicode", value, split, 0)
            .await
            .unwrap();
        let StoredOutput::External { preview, .. } = &stored else {
            panic!("expected stored output")
        };
        assert_eq!(preview.bytes.len(), split);
        assert!(std::str::from_utf8(&preview.bytes).is_err());
        let output = model_stored_output(stored).unwrap();
        assert_eq!(output["External"]["preview"]["text"], "{\"text\":\"");
        assert_eq!(output["External"]["preview"]["truncated"], true);
        assert!(output.pointer("/External/preview/bytes").is_none());
    }
}

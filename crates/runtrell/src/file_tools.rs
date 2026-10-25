// SPDX-License-Identifier: Apache-2.0
//! Standard tools are ordinary Tool adapters; Runtime is the invocation authority.
use crate::{data::*, *};
use serde_json::{Value, json};
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
impl<S: StandardToolStore> FileTool<S> {
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
        let text = json!({"type":"string"});
        let integer = json!({"type":"integer"});
        let (properties, required) = match self.action {
            FileAction::List => (json!({"limit":integer,"cursor":text}), vec!["limit"]),
            FileAction::Read => (
                json!({"path":text,"revision":integer,"offset":integer,"length":integer}),
                vec!["path", "revision", "offset", "length"],
            ),
            FileAction::Write => (
                json!({"path":text,"expected_revision":{"description":"Current integer revision; explicit null to create a new file"},"text":text}),
                vec!["path", "expected_revision", "text"],
            ),
            FileAction::Delete => (
                json!({"path":text,"expected_revision":integer}),
                vec!["path", "expected_revision"],
            ),
            FileAction::Search => (
                json!({"query":text,"cursor":text,"limit":integer,"max_bytes":integer}),
                vec!["query", "limit", "max_bytes"],
            ),
            FileAction::ExactReplace => (
                json!({"expected_revision":integer,"edits":{"type":"array","items":{"type":"object","properties":{"path":text,"expected_digest":text,"old_text":text,"new_text":text},"required":["path","expected_digest","old_text","new_text"],"additionalProperties":false}}}),
                vec!["expected_revision", "edits"],
            ),
            FileAction::ArtifactRead => (
                json!({"artifact":{"type":"object","description":"Saved ArtifactRef returned by an earlier tool"},"offset":integer,"length":integer}),
                vec!["artifact", "offset", "length"],
            ),
        };
        json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
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
    async fn execute(&self, input: Value) -> Result<Value> {
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
            FileAction::List => Ok(json!(
                self.store
                    .list(&self.access, cursor(), size("limit")?)
                    .await?
            )),
            FileAction::Read => Ok(json!(
                self.store
                    .read(
                        &self.access,
                        string("path")?,
                        number("revision")?,
                        number("offset")?,
                        size("length")?
                    )
                    .await?
            )),
            FileAction::Write => {
                let expected = match input.get("expected_revision") {
                    Some(Value::Null) => None,
                    Some(v) => Some(
                        v.as_u64()
                            .ok_or_else(|| Error::Invalid("expected_revision".into()))?,
                    ),
                    None => return Err(Error::Invalid("expected_revision required".into())),
                };
                Ok(json!(
                    self.store
                        .write(
                            &self.access,
                            string("path")?,
                            expected,
                            string("text")?.as_bytes()
                        )
                        .await?
                ))
            }
            FileAction::Delete => {
                self.store
                    .delete(&self.access, string("path")?, number("expected_revision")?)
                    .await?;
                Ok(json!({"saved":true}))
            }
            FileAction::Search => Ok(json!(
                self.store
                    .search(
                        &self.access,
                        string("query")?,
                        cursor(),
                        size("limit")?,
                        size("max_bytes")?
                    )
                    .await?
            )),
            FileAction::ExactReplace => {
                let edits = serde_json::from_value(input["edits"].clone())
                    .map_err(|e| Error::Invalid(e.to_string()))?;
                Ok(json!(
                    self.store
                        .exact_replace(&self.access, number("expected_revision")?, edits)
                        .await?
                ))
            }
            FileAction::ArtifactRead => {
                let reference = serde_json::from_value(input["artifact"].clone())
                    .map_err(|e| Error::Invalid(e.to_string()))?;
                Ok(json!(
                    self.store
                        .read_artifact(&self.access, &reference, number("offset")?, size("length")?)
                        .await?
                ))
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
            version: "runtrell-files-1".into(),
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
            timeout: 30,
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
                max_elapsed: 30,
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
                code: "INPUT_LIMIT".into(),
                usage: Usage::measured(0, 0, 0),
                retry_after: None,
            });
        }
        Ok(match self.execute(input).await {
            Ok(output) => ExternalResult::Applied {
                output,
                usage: Usage::measured(0, 0, 0),
                external_id: None,
            },
            Err(e) => ExternalResult::Failed {
                code: e.to_string(),
                usage: Usage::measured(0, 0, 0),
                retry_after: None,
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
                output,
                usage,
                external_id,
            } => {
                validate_schema(&output, &self.inner.descriptor().output_schema)?;
                ExternalResult::Applied {
                    output: json!(
                        self.store
                            .store_output(
                                &self.access,
                                &context.operation,
                                output,
                                self.inline_limit,
                                self.now
                            )
                            .await?
                    ),
                    usage,
                    external_id,
                }
            }
            other => other,
        })
    }
}
impl<T: Tool, S: StandardToolStore> Tool for StoredOutputTool<T, S> {
    fn descriptor(&self) -> ToolDescriptor {
        let mut d = self.inner.descriptor();
        d.version = format!("{}:stored-output-1:{}", d.version, self.inline_limit);
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
    pub adapter: DataValidator<V, S>,
    pub run: RunRecord,
}
impl<V: TargetValidator, S: EvidenceStore + StandardToolStore> Tool for ValidationTool<V, S> {
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
            timeout: 30,
            max_output_bytes: self.adapter.store.policy().max_read,
            reserve: Amount::default(),
            quantities: Default::default(),
            conflict_scope: Some("validation-evidence".into()),
            retry: RetryPolicy {
                max_attempts: 1,
                backoff: 0,
                max_elapsed: 30,
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
        let receipt = self.adapter.validate(&self.run, &input)?;
        Ok(ExternalResult::Applied {
            output: json!(receipt),
            usage: Usage::measured(0, 0, 0),
            external_id: None,
        })
    }
    async fn lookup(&self, _: &ToolContext, _: Value) -> Result<Lookup> {
        Ok(Lookup::Unknown)
    }
}

// SPDX-License-Identifier: Apache-2.0
//! Optional host-independent assembly helpers. No application presets or connections.
use crate::{data::ValidatorKey, *};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeSet, future::Future, pin::Pin, sync::Arc};

type Call<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;
trait ErasedTool: Send + Sync {
    fn authorize_result<'a>(&'a self, run: &'a RunRecord, output: &'a Value) -> Call<'a, ()>;
    fn quote<'a>(&'a self, run: &'a RunRecord, input: &'a Value) -> Call<'a, Amount>;
    fn operation_kind(&self) -> OperationKind;
    fn descriptor(&self) -> ToolDescriptor;
    fn requires_recovery_preparation(&self) -> bool;
    fn prepare_recovery<'a>(
        &'a self,
        c: &'a PreparationContext,
        v: &'a Value,
    ) -> Call<'a, Option<RecoveryProof>>;
    fn invoke<'a>(&'a self, c: &'a ToolContext, v: Value) -> Call<'a, ExternalResult>;
    fn lookup<'a>(&'a self, c: &'a ToolContext, v: Value) -> Call<'a, Lookup>;
    fn cancel<'a>(&'a self, c: &'a ToolContext, v: Value) -> Call<'a, Progress>;
}
struct Erase<T>(T);
impl<T: Tool> ErasedTool for Erase<T> {
    fn authorize_result<'a>(&'a self, run: &'a RunRecord, output: &'a Value) -> Call<'a, ()> {
        Box::pin(self.0.authorize_result(run, output))
    }
    fn quote<'a>(&'a self, run: &'a RunRecord, input: &'a Value) -> Call<'a, Amount> {
        Box::pin(self.0.quote(run, input))
    }
    fn operation_kind(&self) -> OperationKind {
        self.0.operation_kind()
    }
    fn requires_recovery_preparation(&self) -> bool {
        self.0.requires_recovery_preparation()
    }
    fn prepare_recovery<'a>(
        &'a self,
        c: &'a PreparationContext,
        v: &'a Value,
    ) -> Call<'a, Option<RecoveryProof>> {
        Box::pin(self.0.prepare_recovery(c, v))
    }
    fn descriptor(&self) -> ToolDescriptor {
        self.0.descriptor()
    }
    fn invoke<'a>(&'a self, c: &'a ToolContext, v: Value) -> Call<'a, ExternalResult> {
        Box::pin(self.0.invoke(c, v))
    }
    fn lookup<'a>(&'a self, c: &'a ToolContext, v: Value) -> Call<'a, Lookup> {
        Box::pin(self.0.lookup(c, v))
    }
    fn cancel<'a>(&'a self, c: &'a ToolContext, v: Value) -> Call<'a, Progress> {
        Box::pin(self.0.cancel(c, v))
    }
}
/// Heterogeneous Tool implementations can share the driver's ordinary Vec<T>.
#[derive(Clone)]
pub struct RegisteredTool(Arc<dyn ErasedTool>);
impl RegisteredTool {
    pub fn new(tool: impl Tool + 'static) -> Self {
        Self(Arc::new(Erase(tool)))
    }
}
impl Tool for RegisteredTool {
    async fn authorize_result(&self, run: &RunRecord, output: &Value) -> Result<()> {
        self.0.authorize_result(run, output).await
    }
    async fn quote(&self, run: &RunRecord, input: &Value) -> Result<Amount> {
        self.0.quote(run, input).await
    }
    fn operation_kind(&self) -> OperationKind {
        self.0.operation_kind()
    }
    fn requires_recovery_preparation(&self) -> bool {
        self.0.requires_recovery_preparation()
    }
    async fn prepare_recovery(
        &self,
        c: &PreparationContext,
        v: &Value,
    ) -> Result<Option<RecoveryProof>> {
        self.0.prepare_recovery(c, v).await
    }
    fn descriptor(&self) -> ToolDescriptor {
        self.0.descriptor()
    }
    async fn invoke(&self, c: &ToolContext, v: Value) -> Result<ExternalResult> {
        self.0.invoke(c, v).await
    }
    async fn lookup(&self, c: &ToolContext, v: Value) -> Result<Lookup> {
        self.0.lookup(c, v).await
    }
    async fn cancel(&self, c: &ToolContext, v: Value) -> Result<Progress> {
        self.0.cancel(c, v).await
    }
}
pub struct VersionedValidator {
    pub key: ValidatorKey,
    pub validator: Arc<dyn Validator>,
}
pub struct ValidationChain {
    pub validators: Vec<VersionedValidator>,
    pub schema: Value,
    pub repair: RepairPolicy,
    pub repairable_codes: BTreeSet<String>,
}
/// Verify provenance before accepting either passing or failing diagnostics.
pub fn verify_receipt(
    run: &RunRecord,
    value: &Value,
    receipt: &ValidationReceipt,
    key: &ValidatorKey,
) -> Result<()> {
    if receipt.hash != digest(value)
        || receipt.basis_revision != run.spec.basis_revision
        || receipt.config_version != run.spec.config_version
        || receipt.validator != key.name
        || receipt.validator_version != key.version
    {
        return Err(Error::Conflict);
    }
    Ok(())
}
impl Validator for ValidationChain {
    fn repair_policy(&self) -> RepairPolicy {
        self.repair.clone()
    }
    fn repairable(&self, diagnostics: &[Diagnostic]) -> bool {
        !diagnostics.is_empty()
            && diagnostics.iter().all(|d| {
                matches!(d.severity.as_str(), "info" | "warning")
                    || (d.severity == "error" && self.repairable_codes.contains(&d.code))
            })
            && diagnostics.iter().any(|d| d.severity == "error")
    }
    fn validate(&self, run: &RunRecord, value: &Value) -> Result<ValidationReceipt> {
        let mut diagnostics = schema_diagnostics(value, &self.schema)?;
        let mut passed = diagnostics.is_empty();
        for validator in &self.validators {
            let receipt = validator.validator.validate(run, value)?;
            verify_receipt(run, value, &receipt, &validator.key)?;
            passed &= receipt.passed;
            diagnostics.extend(receipt.diagnostics);
        }
        Ok(ValidationReceipt {
            hash: digest(value),
            basis_revision: run.spec.basis_revision,
            config_version: run.spec.config_version.clone(),
            validator: run.spec.validator.clone(),
            validator_version: run.spec.validator_version.clone(),
            passed,
            diagnostics,
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigDiagnostic {
    pub code: String,
    pub location: String,
    pub message: String,
    pub fix: String,
}
/// Bounds are supplied by the host; no application-specific defaults.
pub struct SettingBound {
    pub name: String,
    pub value: u64,
    pub minimum: u64,
    pub maximum: u64,
}
pub struct AssemblyConfig {
    pub tools: Vec<ToolDescriptor>,
    pub validators: Vec<ValidatorKey>,
    pub required_validators: Vec<String>,
    pub available_capabilities: BTreeSet<String>,
    pub required_capabilities: BTreeSet<String>,
    pub bounds: Vec<SettingBound>,
}
impl AssemblyConfig {
    /// Collect all independent configuration failures; do not stop at the first.
    pub fn diagnostics(&self) -> Vec<ConfigDiagnostic> {
        let mut out = Vec::new();
        let mut add = |code: &str, location: &str, message: &str, fix: &str| {
            out.push(ConfigDiagnostic {
                code: code.into(),
                location: location.into(),
                message: message.into(),
                fix: fix.into(),
            })
        };
        let mut names = BTreeSet::new();
        for tool in &self.tools {
            if !names.insert(&tool.name) {
                add(
                    "TOOL_NAME_CONFLICT",
                    &tool.name,
                    "Duplicate tool name",
                    "Register each name once",
                );
            }
            if tool.name.is_empty()
                || tool.version.is_empty()
                || tool.account.is_empty()
                || tool.permission.is_empty()
                || tool.capability.is_empty()
                || tool.timeout == 0
                || tool.max_output_bytes == 0
            {
                add(
                    "TOOL_CONFIG_INVALID",
                    &tool.name,
                    "Missing identity, authority or positive limits",
                    "Supply explicit valid settings",
                );
            }
            for (name, schema) in [
                ("input_schema", &tool.input_schema),
                ("output_schema", &tool.output_schema),
            ] {
                if let Err(error) = schema::compile_schema(schema) {
                    add(
                        "SCHEMA_INVALID",
                        &format!("{}.{name}", tool.name),
                        &error.to_string(),
                        "Use the supported schema dialect",
                    );
                }
            }
            if !self.available_capabilities.contains(&tool.capability) {
                add(
                    "CAPABILITY_MISSING",
                    &tool.name,
                    "Tool capability unavailable",
                    "Connect a capable adapter",
                );
            }
        }
        let mut validators = BTreeSet::new();
        for key in &self.validators {
            if !validators.insert(&key.name) {
                add(
                    "VALIDATOR_NAME_CONFLICT",
                    &key.name,
                    "Duplicate validator name",
                    "Register each name once",
                );
            }
            if key.name.is_empty() || key.version.is_empty() {
                add(
                    "VALIDATOR_CONFIG_INVALID",
                    &key.name,
                    "Missing validator identity/version",
                    "Pin the implementation version",
                );
            }
        }
        let mut required = BTreeSet::new();
        for name in &self.required_validators {
            if !required.insert(name) {
                add(
                    "VALIDATOR_NAME_CONFLICT",
                    name,
                    "Repeated required validator",
                    "Require each name once",
                );
            }
            if !validators.contains(name) {
                add(
                    "VALIDATOR_NOT_REGISTERED",
                    name,
                    "Required validator missing",
                    "Register the validator",
                );
            }
        }
        for capability in self
            .required_capabilities
            .difference(&self.available_capabilities)
        {
            add(
                "CAPABILITY_MISSING",
                capability,
                "Required capability unavailable",
                "Connect a capable adapter",
            );
        }
        for bound in &self.bounds {
            if bound.minimum > bound.maximum
                || bound.value < bound.minimum
                || bound.value > bound.maximum
            {
                add(
                    "SETTING_OUT_OF_RANGE",
                    &bound.name,
                    "Setting outside host bounds",
                    "Choose a value inside both technical and quality bounds",
                );
            }
        }
        out
    }
}
/// Normalize every object recursively; array order remains semantically significant.
/// Pass the configuration body without a self-referential digest field.
pub fn configuration_digest(value: &Value) -> String {
    let mut value = value.clone();
    value.sort_all_objects();
    digest(&value)
}

/// Preserve the exact validated candidate and receipt across a host finalizer.
/// The host's finalizer still owns publication; this wrapper checks its result.
pub struct CheckedFinalizer<'a>(pub &'a dyn Finalizer);
impl Finalizer for CheckedFinalizer<'_> {
    fn finalize(
        &self,
        run: &RunRecord,
        candidate: Value,
        receipt: ValidationReceipt,
    ) -> Result<Completion> {
        verify_receipt(
            run,
            &candidate,
            &receipt,
            &ValidatorKey::new(&run.spec.validator, &run.spec.validator_version),
        )?;
        if !receipt.passed {
            return Err(Error::Invalid("VALIDATION_FAILED".into()));
        }
        let completion = self.0.finalize(run, candidate.clone(), receipt.clone())?;
        if completion.id.is_empty()
            || completion.output != candidate
            || completion.receipt != receipt
        {
            return Err(Error::Conflict);
        }
        Ok(completion)
    }
}

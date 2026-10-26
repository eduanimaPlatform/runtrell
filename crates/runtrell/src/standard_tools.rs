// SPDX-License-Identifier: Apache-2.0
//! Opt-in standard catalog assembly. Cargo features only make implementations
//! available; ToolSet chooses definitions, pin_catalog filters Run permissions,
//! and Runtime/Policy authorize every call again. Extensions use RegisteredTool.
use crate::{assembly::*, data::*, file_tools::*, *};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum StandardTool {
    List,
    Read,
    Write,
    Delete,
    Search,
    ExactReplace,
    ArtifactRead,
    Validation,
    Execution,
    ImageGenerate,
    ImageEdit,
    DocumentView,
}
impl StandardTool {
    pub fn requirements(self) -> &'static [&'static str] {
        match self {
            Self::DocumentView => &["StandardToolStore", "Access", "Policy", "RunStore", "Clock"],
            Self::ImageGenerate | Self::ImageEdit => &[
                "ImagePort",
                "ArtifactStore",
                "Access",
                "Policy",
                "RunStore",
                "Clock",
            ],
            Self::ArtifactRead => &["ArtifactStore", "Access"],
            Self::Validation => &["Validator", "EvidenceStore", "Access", "RunRecord"],
            Self::Execution => &[
                "WorkspaceStore",
                "ArtifactStore",
                "Access",
                "ExecutionBackend",
                "ExecutionLedger",
                "ControlStore",
                "Policy",
                "Clock",
                "ExecutionToolConfig",
            ],
            _ => &["WorkspaceStore", "Access"],
        }
    }
    fn action(self) -> Option<FileAction> {
        Some(match self {
            Self::List => FileAction::List,
            Self::Read => FileAction::Read,
            Self::Write => FileAction::Write,
            Self::Delete => FileAction::Delete,
            Self::Search => FileAction::Search,
            Self::ExactReplace => FileAction::ExactReplace,
            Self::ArtifactRead => FileAction::ArtifactRead,
            _ => return None,
        })
    }
}
/// A preset is only a map of selected tools and their default settings. Calling
/// select/disable/settings after applying it overwrites that individual entry.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ToolPreset(pub BTreeMap<StandardTool, ToolSettings>);
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ToolSettings {
    pub timeout: Option<u64>,
    pub max_output_bytes: Option<usize>,
    pub reserve: Option<Amount>,
    /// None means no large-output adapter. Requires an explicit store and Access.
    pub stored_output: Option<StoredOutputSettings>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredOutputSettings {
    pub inline_bytes: usize,
    pub now: u64,
}
struct ConfiguredTool {
    inner: RegisteredTool,
    descriptor: ToolDescriptor,
}
impl Tool for ConfiguredTool {
    async fn authorize_result(&self, run: &RunRecord, output: &serde_json::Value) -> Result<()> {
        self.inner.authorize_result(run, output).await
    }
    async fn quote(&self, run: &RunRecord, input: &serde_json::Value) -> Result<Amount> {
        let amount = self.inner.quote(run, input).await?;
        Ok(Amount::new(
            amount.tokens.max(self.descriptor.reserve.tokens),
            amount.micro_usd.max(self.descriptor.reserve.micro_usd),
        ))
    }
    fn operation_kind(&self) -> OperationKind {
        self.inner.operation_kind()
    }
    fn requires_recovery_preparation(&self) -> bool {
        self.inner.requires_recovery_preparation()
    }
    async fn prepare_recovery(
        &self,
        c: &PreparationContext,
        v: &serde_json::Value,
    ) -> Result<Option<RecoveryProof>> {
        self.inner.prepare_recovery(c, v).await
    }
    fn descriptor(&self) -> ToolDescriptor {
        self.descriptor.clone()
    }
    async fn invoke(&self, c: &ToolContext, v: serde_json::Value) -> Result<ExternalResult> {
        self.inner.invoke(c, v).await
    }
    async fn lookup(&self, c: &ToolContext, v: serde_json::Value) -> Result<Lookup> {
        self.inner.lookup(c, v).await
    }
    async fn cancel(&self, c: &ToolContext, v: serde_json::Value) -> Result<Progress> {
        self.inner.cancel(c, v).await
    }
}
/// Empty by default. Supplying resources does not select any tool.
pub struct ToolSet<S = DataStore> {
    selected: BTreeMap<StandardTool, ToolSettings>,
    files: Option<(S, Access)>,
    supplied: BTreeMap<StandardTool, RegisteredTool>,
    diagnostics: BTreeMap<StandardTool, Vec<ConfigDiagnostic>>,
}
impl<S> Default for ToolSet<S> {
    fn default() -> Self {
        Self {
            selected: BTreeMap::new(),
            files: None,
            supplied: BTreeMap::new(),
            diagnostics: BTreeMap::new(),
        }
    }
}
impl<S: StandardToolStore + Clone + 'static> ToolSet<S> {
    pub fn preset(mut self, preset: ToolPreset) -> Self {
        self.selected.extend(preset.0);
        self
    }
    pub fn select(mut self, tool: StandardTool) -> Self {
        self.selected.entry(tool).or_default();
        self
    }
    pub fn disable(mut self, tool: StandardTool) -> Self {
        self.selected.remove(&tool);
        self
    }
    pub fn settings(mut self, tool: StandardTool, settings: ToolSettings) -> Self {
        self.selected.insert(tool, settings);
        self
    }
    pub fn files(mut self, store: S, access: Access) -> Self {
        self.files = Some((store, access));
        self
    }
    pub fn image<
        I: images::ImagePort + 'static,
        P: Policy + 'static,
        R: RunStore + 'static,
        C: Clock + 'static,
    >(
        mut self,
        tool: images::ImageTool<I, S, P, R, C>,
    ) -> Self {
        let kind = if tool.config.action == images::ImageAction::Generate {
            StandardTool::ImageGenerate
        } else {
            StandardTool::ImageEdit
        };
        self.supplied.insert(kind, RegisteredTool::new(tool));
        self
    }
    pub fn validation<V: TargetValidator + 'static>(mut self, tool: ValidationTool<V, S>) -> Self
    where
        S: EvidenceStore,
    {
        self.supplied
            .insert(StandardTool::Validation, RegisteredTool::new(tool));
        self
    }
    pub fn document_view<P: Policy + 'static, R: RunStore + 'static, C: Clock + 'static>(
        mut self,
        tool: document_view::DocumentView<S, P, R, C>,
    ) -> Self {
        self.supplied
            .insert(StandardTool::DocumentView, RegisteredTool::new(tool));
        self
    }
    pub fn execution<
        Cs: ControlStore + 'static,
        W: StandardToolStore + 'static,
        L: execution::ExecutionLedger + 'static,
        B: ExecutionBackend + 'static,
        P: Policy + 'static,
        C: Clock + 'static,
    >(
        mut self,
        tool: execution::ExecutionTool<Cs, W, L, B, P, C>,
    ) -> Self {
        self.diagnostics
            .insert(StandardTool::Execution, tool.diagnostics());
        self.supplied
            .insert(StandardTool::Execution, RegisteredTool::new(tool));
        self
    }
    pub fn build(self) -> std::result::Result<Vec<RegisteredTool>, Vec<ConfigDiagnostic>> {
        let mut errors = Vec::new();
        let mut tools = Vec::new();
        for (kind, settings) in self.selected {
            if let Some(diagnostics) = self.diagnostics.get(&kind) {
                errors.extend(diagnostics.clone());
            }
            let tool = if let Some(action) = kind.action() {
                self.files.as_ref().map(|(store, access)| {
                    RegisteredTool::new(FileTool {
                        store: store.clone(),
                        access: access.clone(),
                        action,
                    })
                })
            } else {
                self.supplied.get(&kind).cloned()
            };
            let Some(mut tool) = tool else {
                errors.push(ConfigDiagnostic {
                    code: if kind == StandardTool::Execution {
                        "EXECUTION_BACKEND_MISSING"
                    } else {
                        "TOOL_RESOURCE_MISSING"
                    }
                    .into(),
                    location: format!("{kind:?}"),
                    message: format!("Required resources: {}", kind.requirements().join(", ")),
                    fix: "Connect the required resources or disable this tool".into(),
                });
                continue;
            };
            if let Some(stored) = settings.stored_output {
                if matches!(kind, StandardTool::ImageGenerate | StandardTool::ImageEdit) {
                    errors.push(ConfigDiagnostic {
                        code: "SETTING_UNSUPPORTED".into(),
                        location: format!("{kind:?}.stored_output"),
                        message: "Image tools already return bounded Artifact references".into(),
                        fix: "Remove the stored_output wrapper for this image tool".into(),
                    });
                    continue;
                }
                if let Some((store, access)) = &self.files {
                    if stored.inline_bytes == 0 {
                        errors.push(ConfigDiagnostic {
                            code: "SETTING_OUT_OF_RANGE".into(),
                            location: format!("{kind:?}.inline_bytes"),
                            message: "Expected a positive inline limit".into(),
                            fix: "Set inline_bytes".into(),
                        });
                    }
                    tool = RegisteredTool::new(StoredOutputTool {
                        inner: tool,
                        store: store.clone(),
                        access: access.clone(),
                        inline_limit: stored.inline_bytes,
                        now: stored.now,
                    });
                } else {
                    errors.push(ConfigDiagnostic {
                        code: "TOOL_RESOURCE_MISSING".into(),
                        location: format!("{kind:?}.stored_output"),
                        message: "ArtifactStore and Access required".into(),
                        fix: "Connect output storage".into(),
                    });
                }
            }
            let mut descriptor = tool.descriptor();
            if let Some(timeout) = settings.timeout {
                descriptor.timeout = timeout;
            }
            if let Some(bytes) = settings.max_output_bytes {
                descriptor.max_output_bytes = bytes;
            }
            if let Some(reserve) = settings.reserve {
                descriptor.reserve = reserve;
            }
            let config = AssemblyConfig {
                tools: vec![descriptor.clone()],
                validators: vec![],
                required_validators: vec![],
                available_capabilities: [descriptor.capability.clone()].into_iter().collect(),
                required_capabilities: Default::default(),
                bounds: vec![],
            };
            errors.extend(config.diagnostics());
            tools.push(RegisteredTool::new(ConfiguredTool {
                inner: tool,
                descriptor,
            }));
        }
        if errors.is_empty() {
            Ok(tools)
        } else {
            Err(errors)
        }
    }
}

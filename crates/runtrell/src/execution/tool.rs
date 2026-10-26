// SPDX-License-Identifier: Apache-2.0
//! Model-callable execution. Compilation, catalog selection and Policy are separate gates.
use super::*;
use crate::data::{Access, ArtifactSpec, DataClass, StandardToolStore};
use controller::{bounded, poll_backend};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionInput {
    pub program: ExecutionProgram,
    /// Exact logical paths. No implicit access to the rest of the workspace.
    pub files: BTreeSet<String>,
    pub outputs: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ExecutionProgram {
    Process {
        executable: String,
        args: Vec<String>,
    },
    #[cfg(feature = "python")]
    Python { script: String },
}
/// Host explicitly permits arbitrary confined arguments or a finite set of vectors.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ArgumentPolicy {
    Any,
    Exact(BTreeSet<Vec<String>>),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionToolConfig {
    /// Explicit allowed executables. Arguments remain confined by the selected backend.
    pub executables: BTreeMap<String, ArgumentPolicy>,
    pub limits: ExecutionLimits,
    pub requires_descendants: bool,
    pub reserve: Amount,
    pub summary_bytes: usize,
}
/// The host connects resources; construction never acquires an execution environment.
/// `control`, `policy` and `clock` must be the same authorities used by Runtime.
pub struct ExecutionTool<S, W, L, B, P, C> {
    pub control: Arc<S>,
    pub workspace: W,
    pub access: Access,
    pub ledger: L,
    pub backend: B,
    pub policy: P,
    pub clock: C,
    pub config: ExecutionToolConfig,
    #[cfg(feature = "python")]
    python: Option<python::PythonRuntime>,
}
impl<
    S: ControlStore,
    W: StandardToolStore,
    L: ExecutionLedger,
    B: ExecutionBackend,
    P: Policy,
    C: Clock,
> ExecutionTool<S, W, L, B, P, C>
{
    pub fn new(
        control: Arc<S>,
        workspace: W,
        access: Access,
        ledger: L,
        backend: B,
        authorities: (P, C),
        config: ExecutionToolConfig,
    ) -> Self {
        Self {
            control,
            workspace,
            access,
            ledger,
            backend,
            policy: authorities.0,
            clock: authorities.1,
            config,
            #[cfg(feature = "python")]
            python: None,
        }
    }
    #[cfg(feature = "python")]
    pub fn python(mut self, runtime: python::PythonRuntime) -> Self {
        self.python = Some(runtime);
        self
    }
    pub fn diagnostics(&self) -> Vec<crate::assembly::ConfigDiagnostic> {
        let mut out = Vec::new();
        let mut request = ExecutionRequest::process("configuration-probe", vec![]);
        request.limits = self.config.limits.clone();
        if let Err(error) = self.backend.capabilities().check(&request) {
            out.push(crate::assembly::ConfigDiagnostic {
                code: "EXECUTION_CAPABILITY_MISSING".into(),
                location: "execution.limits".into(),
                message: error.to_string(),
                fix: "Connect a backend enforcing all requested limits".into(),
            });
        }
        let executables: Vec<_> = self.config.executables.keys().map(String::as_str).collect();
        #[cfg(feature = "python")]
        let executables: Vec<_> = executables
            .into_iter()
            .chain(self.python.as_ref().map(|python| python.executable()))
            .collect();
        for executable in executables {
            let mut request = ExecutionRequest::process(executable, vec![]);
            request.limits = self.config.limits.clone();
            request.requires_descendants = self.config.requires_descendants;
            if let Err(error) = self.backend.supports(&request) {
                out.push(crate::assembly::ConfigDiagnostic {
                    code: "EXECUTION_CAPABILITY_MISSING".into(),
                    location: format!("execution.executables.{executable}"),
                    message: error.to_string(),
                    fix: "Connect a compatible backend or change the explicit configuration".into(),
                });
            }
        }
        if self.config.summary_bytes == 0 || self.config.summary_bytes > 65_536 {
            out.push(crate::assembly::ConfigDiagnostic {
                code: "SETTING_OUT_OF_RANGE".into(),
                location: "execution.summary_bytes".into(),
                message: "Expected 1..=65536 bytes".into(),
                fix: "Supply a bounded preview size".into(),
            });
        }
        out
    }
    async fn check(&self, c: &ToolContext) -> Result<()> {
        if c.claim.run != self.access.actor().owner
            || self.access.actor() != self.access.namespace()
            || c.account_binding != self.descriptor().account
        {
            return Err(Error::PermissionDenied);
        }
        let run = self.control.check(&c.claim, self.clock.now(), true).await?;
        self.policy.authorize(&run, "execute", "execute")
    }
    async fn request(&self, input: Value) -> Result<ExecutionRequest> {
        let input: ExecutionInput =
            serde_json::from_value(input).map_err(|e| Error::Invalid(e.to_string()))?;
        if input.files.len() > 4096 || input.outputs.len() > 256 {
            return Err(Error::Invalid("COPY_LIMIT".into()));
        }
        for path in input.files.iter().chain(input.outputs.iter()) {
            crate::data::logical_path(path)?;
        }
        let mut request = ExecutionRequest::process("", vec![]);
        // Pin the entire CAS basis, but read only the explicitly selected files.
        let base = self.workspace.snapshot(&self.access).await?;
        if base.files.len() > 4096 {
            return Err(Error::Invalid("COPY_LIMIT".into()));
        }
        let mut snapshot = base.clone();
        snapshot.files.retain(|f| input.files.contains(&f.path));
        if snapshot.files.len() != input.files.len() {
            return Err(Error::NotFound);
        }
        let mut total = 0usize;
        for file in &snapshot.files {
            let mut bytes = Vec::new();
            while bytes.len() < file.bytes as usize {
                let part = self
                    .workspace
                    .read(
                        &self.access,
                        &file.path,
                        file.revision,
                        bytes.len() as u64,
                        65_536,
                    )
                    .await?;
                if part.bytes.is_empty() || part.total != file.bytes {
                    return Err(Error::Conflict);
                }
                total = total.checked_add(part.bytes.len()).ok_or(Error::Conflict)?;
                if total > 16_777_216 {
                    return Err(Error::Invalid("COPY_LIMIT".into()));
                }
                bytes.extend(part.bytes);
            }
            request.input_files.insert(file.path.clone(), bytes);
        }
        if self.workspace.snapshot(&self.access).await? != base {
            return Err(Error::Conflict);
        }
        snapshot.hash = digest(&json!(snapshot.files));
        request.input = Some(snapshot);
        request.workspace_base = Some(base);
        request.program = match input.program {
            ExecutionProgram::Process { executable, args } => {
                match self.config.executables.get(&executable) {
                    Some(ArgumentPolicy::Any) => {}
                    Some(ArgumentPolicy::Exact(allowed)) if allowed.contains(&args) => {}
                    _ => return Err(Error::PermissionDenied),
                }
                Program::Process { executable, args }
            }
            #[cfg(feature = "python")]
            ExecutionProgram::Python { script } => {
                let python = self
                    .python
                    .as_ref()
                    .ok_or_else(|| Error::Unsupported("PYTHON_NOT_CONFIGURED".into()))?;
                let python = python.clone();
                let frozen = (
                    request.input.clone().ok_or(Error::Conflict)?,
                    request.input_files.clone(),
                );
                let outputs = input.outputs.clone();
                let limits = self.config.limits.clone();
                let built = tokio::task::spawn_blocking(move || {
                    python.request(
                        frozen,
                        &script,
                        outputs,
                        limits,
                        python::DependencyPolicy::PreparedOnly,
                    )
                })
                .await
                .map_err(|_| Error::Storage)??;
                request.executable_hash = built.executable_hash;
                built.program
            }
        };
        request.output_paths = input.outputs;
        request.limits = self.config.limits.clone();
        request.requires_descendants = self.config.requires_descendants;
        request.validate_input(&self.access.actor().owner)?;
        self.backend.supports(&request)?;
        Ok(request)
    }
    async fn finish(&self, c: &ToolContext) -> Result<Lookup> {
        let result = poll_backend(&self.ledger, &self.backend, c, &self.clock).await?;
        let Lookup::Found(result) = result else {
            return Ok(result);
        };
        let record = self.ledger.get(&c.claim.run, &c.operation).await?;
        let raw = record
            .observation
            .as_ref()
            .and_then(|o| o.result.clone())
            .unwrap_or(Value::Null);
        let bytes = serde_json::to_vec(&raw).map_err(|_| Error::Storage)?;
        if bytes.len() > 100_663_296 {
            return Err(Error::Invalid("OUTPUT_LIMIT".into()));
        }
        let staged = self
            .workspace
            .stage_named(
                &self.access,
                ArtifactSpec {
                    id: format!("execution-{}", digest(&json!([c.operation, raw]))),
                    media_type: "application/json".into(),
                    class: DataClass::Intermediate,
                },
                &bytes,
                self.clock.now(),
            )
            .await?;
        let artifact = self
            .workspace
            .save(&self.access, staged, self.clock.now())
            .await?;
        let (usage, failure) = match result {
            ExternalResult::Applied { usage, .. } => (usage, None),
            ExternalResult::Failed { usage, code, .. } => (usage, Some(code)),
            _ => return Err(Error::Conflict),
        };
        // Cleanup and cost can settle even when saving conflicts. Retain output in Artifact
        // and ledger, and never describe a failed or ambiguous merge as saved.
        let manifest_valid = raw
            .get("files")
            .and_then(Value::as_object)
            .is_some_and(|files| {
                files.len() <= 256
                    && files
                        .keys()
                        .all(|path| record.request.output_paths.contains(path))
            });
        let saved = if failure.is_none() && manifest_valid {
            match self.check(c).await {
                Ok(()) => merge_process_copy(&self.workspace, &self.access, &record).await,
                Err(error) => Err(error),
            }
        } else {
            Err(Error::Closed)
        };
        let mut output = json!({"artifact":artifact, "exit":raw["exit"], "failure":failure,
            "stdout":preview(&raw["stdout"], self.config.summary_bytes),
            "stderr":preview(&raw["stderr"], self.config.summary_bytes)});
        match saved {
            Ok(snapshot) => {
                output["saved"] = json!(true);
                output["workspace_revision"] = json!(snapshot.revision);
                output["files"] = json!(
                    snapshot
                        .files
                        .into_iter()
                        .filter(|f| record.request.output_paths.contains(&f.path))
                        .collect::<Vec<_>>()
                );
            }
            Err(error) => {
                output["saved"] = json!(false);
                output["save_error"] = json!(error.to_string());
            }
        }
        Ok(Lookup::Found(ExternalResult::Applied {
            physical_evidence: None,
            output,
            usage,
            external_id: Some(c.operation.clone()),
        }))
    }
}
fn preview(value: &Value, limit: usize) -> Value {
    let bytes = if let Some(s) = value.as_str() {
        s.as_bytes().to_vec()
    } else {
        serde_json::from_value::<Vec<u8>>(value.clone()).unwrap_or_default()
    };
    json!({"text":String::from_utf8_lossy(&bytes[..bytes.len().min(limit)]), "bytes":bytes.len(), "truncated":bytes.len()>limit})
}
impl<
    S: ControlStore,
    W: StandardToolStore,
    L: ExecutionLedger,
    B: ExecutionBackend,
    P: Policy,
    C: Clock,
> Tool for ExecutionTool<S, W, L, B, P, C>
{
    fn operation_kind(&self) -> OperationKind {
        OperationKind::Sandbox
    }
    fn descriptor(&self) -> ToolDescriptor {
        let programs:Vec<_> = self.config.executables.iter().map(|(executable,policy)| {
            let args=match policy {
                ArgumentPolicy::Any => json!({"type":"array","items":{"type":"string"},"maxItems":256}),
                ArgumentPolicy::Exact(allowed) => json!({"type":"array","enum":allowed}),
            };
            json!({"type":"object","properties":{"Process":{"type":"object","properties":{
                "executable":{"type":"string","const":executable},"args":args},
                "required":["executable","args"],"additionalProperties":false}},"required":["Process"],"additionalProperties":false})
        }).collect();
        #[cfg(feature = "python")]
        let programs = {
            let mut programs = programs;
            if self.python.is_some() {
                programs.push(json!({"type":"object","properties":{"Python":{"type":"object","properties":{"script":{"type":"string"}},"required":["script"],"additionalProperties":false}},"required":["Python"],"additionalProperties":false}));
            }
            programs
        };
        let identity = json!([
            "execution-tool-1",
            self.config,
            self.backend.capabilities(),
            self.access.namespace()
        ]);
        #[cfg(feature = "python")]
        let identity = json!([identity, self.python]);
        ToolDescriptor {
            name: "execute".into(),
            version: digest(&identity),
            account: "execution".into(),
            permission: "execute".into(),
            capability: "execution-backend".into(),
            input_schema: json!({"type":"object", "properties":{"program":{"oneOf":programs},"files":{"type":"array","items":{"type":"string"}},"outputs":{"type":"array","items":{"type":"string"}}},"required":["program","files","outputs"],"additionalProperties":false}),
            output_schema: json!({"type":"object"}),
            side_effect: true,
            timeout: self
                .config
                .limits
                .acquire_ms
                .saturating_add(self.config.limits.wall_ms)
                .saturating_add(self.config.limits.stop_ms)
                .saturating_add(self.config.limits.cleanup_ms),
            max_output_bytes: 8_388_608,
            reserve: self.config.reserve,
            quantities: Default::default(),
            conflict_scope: Some(digest(&json!(self.access.namespace()))),
            retry: RetryPolicy {
                max_attempts: 1,
                backoff: 0,
                max_elapsed: 0,
            },
        }
    }
    async fn invoke(&self, c: &ToolContext, input: Value) -> Result<ExternalResult> {
        if c.permission != "execute" {
            return Err(Error::PermissionDenied);
        }
        self.check(c).await?;
        let request = match self.request(input).await {
            Ok(request) => request,
            Err(error) => {
                return Ok(ExternalResult::Failed {
                    physical_evidence: None,
                    code: error.to_string(),
                    usage: Usage::measured(0, 0, 0),
                    retry_after: None,
                });
            }
        };
        let mut request = request;
        if let Some(deadline) = c.deadline {
            request.limits.wall_ms = request.limits.wall_ms.min(
                deadline
                    .checked_sub(self.clock.now())
                    .filter(|n| *n > 0)
                    .ok_or(Error::Deadline)?,
            );
        }
        self.ledger
            .insert(ExecutionRecord::intent(c, request.clone()))
            .await?;
        self.check(c).await?;
        let deadline = c
            .deadline
            .unwrap_or(u64::MAX)
            .min(self.clock.now().saturating_add(request.limits.acquire_ms));
        let execution =
            match bounded(&self.clock, deadline, self.backend.acquire(c, &request)).await {
                Ok(execution) => execution,
                Err(error) => {
                    // Acquisition may finish late. Persist cancellation before attempting stop.
                    request_cancel(&self.ledger, c).await?;
                    let _ = self.cancel(c, Value::Null).await;
                    return Err(error);
                }
            };
        if execution.owner != c.claim.run
            || execution.id != c.operation
            || execution.runtime_version != self.backend.capabilities().runtime_version
        {
            request_cancel(&self.ledger, c).await?;
            return Err(Error::PermissionDenied);
        }
        let cancelled = self.check(c).await.is_err()
            || self
                .ledger
                .get(&c.claim.run, &c.operation)
                .await?
                .cancel_requested;
        if cancelled
            || bounded(
                &self.clock,
                self.clock.now().saturating_add(request.limits.wall_ms),
                self.backend.start(c, &execution),
            )
            .await
            .is_err()
        {
            self.cancel(c, Value::Null).await?;
        }
        Ok(match self.finish(c).await? {
            Lookup::Found(result) => result,
            _ => ExternalResult::Accepted {
                external_id: execution.id,
            },
        })
    }
    async fn lookup(&self, c: &ToolContext, _: Value) -> Result<Lookup> {
        self.control
            .check(&c.claim, self.clock.now(), false)
            .await?;
        // No replacement acquisition or replay of a Ready environment on recovery.
        let record = self.ledger.get(&c.claim.run, &c.operation).await?;
        if bounded(
            &self.clock,
            self.clock
                .now()
                .saturating_add(record.request.limits.cleanup_ms),
            self.backend.lookup(c),
        )
        .await?
        .is_some_and(|o| o.state == ExecutionState::Ready)
        {
            self.cancel(c, Value::Null).await?;
        }
        if self.check(c).await.is_err() {
            self.cancel(c, Value::Null).await?;
        }
        self.finish(c).await
    }
    async fn cancel(&self, c: &ToolContext, _: Value) -> Result<Progress> {
        self.control
            .check(&c.claim, self.clock.now(), false)
            .await?;
        if c.claim.run != self.access.actor().owner {
            return Err(Error::PermissionDenied);
        }
        match request_cancel(&self.ledger, c).await {
            Err(Error::NotFound) => return Ok(Progress::Unknown),
            result => result?,
        }
        let record = self.ledger.get(&c.claim.run, &c.operation).await?;
        let deadline = self
            .clock
            .now()
            .saturating_add(record.request.limits.stop_ms);
        bounded(&self.clock, deadline, self.backend.cancel(c)).await?;
        bounded(&self.clock, deadline, self.backend.stop(c)).await
    }
}

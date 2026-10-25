// SPDX-License-Identifier: Apache-2.0
//! Optional request builder, not a Python interpreter in the Rust control process.
use super::*;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum DependencyPolicy {
    /// Host prepares an immutable runtime/environment, including any wheels, offline.
    #[default]
    PreparedOnly,
    /// Reserved policy shape. No downloader is implemented; always rejected.
    Fetch {
        network_allow: Vec<String>,
        credential_scopes: Vec<String>,
        budget_micro_usd: u64,
        deadline_ms: u64,
    },
}
impl DependencyPolicy {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::PreparedOnly => Ok(()),
            Self::Fetch { .. } => Err(Error::Unsupported("PYTHON_DEPENDENCY_FETCH".into())),
        }
    }
}
#[derive(Clone, Debug)]
pub struct PythonRuntime {
    executable: String,
    /// Host-verified interpreter version, bound into request arguments.
    version: String,
    hash: String,
}
impl PythonRuntime {
    pub fn new(path: &str, version: &str, hash: &str) -> Result<Self> {
        if !Path::new(path).is_absolute()
            || version.is_empty()
            || hash.len() != 64
            || !hash.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(Error::Invalid("PYTHON_RUNTIME_IDENTITY".into()));
        }
        Ok(Self {
            executable: path.into(),
            version: version.into(),
            hash: hash.into(),
        })
    }
    /// Requires a frozen Workspace. Use execute_backend, then merge_process_copy
    /// with the original snapshot; execution success is not a storage receipt.
    pub fn request(
        &self,
        frozen: (WorkspaceSnapshot, BTreeMap<String, Vec<u8>>),
        script: &str,
        outputs: Vec<String>,
        limits: ExecutionLimits,
        dependencies: DependencyPolicy,
    ) -> Result<ExecutionRequest> {
        dependencies.validate()?;
        crate::data::logical_path(script)?;
        if !frozen.1.contains_key(script) {
            return Err(Error::NotFound);
        }
        if crate::data::content_digest(
            &std::fs::read(&self.executable).map_err(|_| Error::Storage)?,
        ) != self.hash
        {
            return Err(Error::Conflict);
        }
        // -I ignores user site/PYTHON*; -B prevents implicit bytecode writes.
        // Check the host's version assertion inside confinement before running the script.
        let bootstrap = "import sys,runpy; expected,script=sys.argv[1:3]; assert '.'.join(map(str,sys.version_info[:3]))==expected, 'PYTHON_VERSION'; sys.argv=[script]; runpy.run_path(script,run_name='__main__')";
        let mut request = ExecutionRequest::process(
            &self.executable,
            vec![
                "-I".into(),
                "-B".into(),
                "-c".into(),
                bootstrap.into(),
                self.version.clone(),
                script.into(),
            ],
        );
        request.executable_hash = Some(self.hash.clone());
        request.input = Some(frozen.0);
        request.input_files = frozen.1;
        request.output_paths = outputs;
        request.limits = limits;
        request.validate_input(
            &request
                .input
                .as_ref()
                .ok_or(Error::Conflict)?
                .namespace
                .owner,
        )?;
        Capabilities {
            isolation: Isolation::Untrusted,
            cpu: true,
            ram: true,
            pids: true,
            disk: true,
            file: true,
            network_deny: true,
            runtime_version: String::new(),
        }
        .check(&request)?;
        Ok(request)
    }
}

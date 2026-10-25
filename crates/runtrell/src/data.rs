// SPDX-License-Identifier: Apache-2.0
//! Owned data, validation evidence, and context. No model supplied authority.
use crate::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
mod local;
use local::Local;

pub fn content_digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}
fn binary_text(text: &str) -> bool {
    text.chars()
        .any(|c| c.is_control() && !['\r', '\n', '\t'].contains(&c))
}
fn invalid(code: &str) -> Error {
    Error::Invalid(code.into())
}
/// Portable ASCII paths; case aliases are rejected when building a manifest.
pub fn logical_path(path: &str) -> Result<()> {
    if path.len() > 1024
        || path.split('/').any(|p| {
            p.is_empty()
                || p == "."
                || p == ".."
                || p.len() > 128
                || !p
                    .bytes()
                    .all(|b| b.is_ascii_alphabetic() || b.is_ascii_digit() || b"._-".contains(&b))
        })
    {
        return Err(invalid("PATH_NOT_CANONICAL"));
    }
    Ok(())
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Namespace {
    pub owner: RunHandle,
    pub agent: String,
}
impl Namespace {
    fn key(&self) -> Result<String> {
        for s in [&self.owner.tenant, &self.owner.id, &self.agent] {
            logical_path(s)?;
            if s.contains('/') {
                return Err(invalid("IDENTITY"));
            }
        }
        Ok(digest(&json!(self)))
    }
}
/// Constructed by a trusted host from the authenticated Run and Agent, never tool arguments.
#[derive(Clone, Debug)]
pub struct Access {
    actor: Namespace,
    target: Namespace,
    grant: Option<String>,
}
impl Access {
    pub fn owner(ns: Namespace) -> Self {
        Self {
            actor: ns.clone(),
            target: ns,
            grant: None,
        }
    }
    pub fn for_actor(actor: Namespace, target: Namespace) -> Self {
        Self {
            actor,
            target,
            grant: None,
        }
    }
    pub fn actor(&self) -> &Namespace {
        &self.actor
    }
    pub fn namespace(&self) -> &Namespace {
        &self.target
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileVersion {
    pub path: String,
    pub revision: u64,
    pub hash: String,
    pub bytes: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSnapshot {
    pub namespace: Namespace,
    pub revision: u64,
    pub files: Vec<FileVersion>,
    pub hash: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Change {
    Write {
        path: String,
        expected: Option<u64>,
        bytes: Vec<u8>,
    },
    Delete {
        path: String,
        expected: u64,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExactEdit {
    pub path: String,
    pub expected_digest: String,
    pub old_text: String,
    pub new_text: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page<T> {
    pub entries: Vec<T>,
    pub next: Option<String>,
    pub truncated: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ByteRange {
    pub bytes: Vec<u8>,
    pub offset: u64,
    pub total: u64,
    pub truncated: bool,
}
fn range(bytes: &[u8], offset: u64, length: usize, max: usize) -> Result<ByteRange> {
    if length == 0 || length > max {
        return Err(invalid("RANGE_LIMIT"));
    }
    let start = usize::try_from(offset).map_err(|_| invalid("RANGE"))?;
    if start > bytes.len() {
        return Err(invalid("RANGE"));
    }
    let end = start.saturating_add(length).min(bytes.len());
    Ok(ByteRange {
        bytes: bytes[start..end].to_vec(),
        offset,
        total: bytes.len() as u64,
        truncated: end < bytes.len(),
    })
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Match {
    pub path: String,
    pub line: usize,
    pub text: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum DataClass {
    Workspace,
    Intermediate,
    Final,
    Checkpoint,
    Log,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClassPolicy {
    pub max_bytes: usize,
    pub retention: u64,
    pub deletable: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoragePolicy {
    pub classes: BTreeMap<DataClass, ClassPolicy>,
    pub max_read: usize,
    pub max_entries: usize,
    pub max_edits: usize,
    pub max_edit_bytes: usize,
}
impl Default for StoragePolicy {
    fn default() -> Self {
        Self {
            classes: [
                DataClass::Workspace,
                DataClass::Intermediate,
                DataClass::Final,
                DataClass::Checkpoint,
                DataClass::Log,
            ]
            .into_iter()
            .map(|c| {
                (
                    c,
                    ClassPolicy {
                        max_bytes: 16 * 1024 * 1024,
                        retention: 86_400_000,
                        deletable: true,
                    },
                )
            })
            .collect(),
            max_read: 256 * 1024,
            max_entries: 1000,
            max_edits: 64,
            max_edit_bytes: 1024 * 1024,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Staged {
    pub id: String,
    owner: Namespace,
    bytes: Vec<u8>,
    class: DataClass,
    expires_at: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub owner: Namespace,
    pub id: String,
    pub revision: u64,
    pub hash: String,
    pub bytes: u64,
    pub manifest: Vec<FileVersion>,
    pub retention_until: u64,
    pub class: DataClass,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Blob {
    reference: ArtifactRef,
    bytes: Vec<u8>,
    completion: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct File {
    version: FileVersion,
    bytes: Vec<u8>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Grant {
    actor: Namespace,
    revision: u64,
    artifact_revision: u64,
    context_revision: u64,
    write: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidatorKey {
    pub name: String,
    pub version: String,
}
impl ValidatorKey {
    pub fn new(name: &str, version: &str) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Target {
    pub snapshot: Option<WorkspaceSnapshot>,
    pub artifacts: Vec<ArtifactRef>,
    pub value: Value,
    pub basis_revision: u64,
    pub config_version: String,
}
impl Target {
    pub fn structured(value: Value, basis_revision: u64, config: &str) -> Self {
        Self {
            snapshot: None,
            artifacts: vec![],
            value,
            basis_revision,
            config_version: config.into(),
        }
    }
    pub fn artifact(
        snapshot: WorkspaceSnapshot,
        value: Value,
        basis_revision: u64,
        config: &str,
    ) -> Self {
        Self {
            snapshot: Some(snapshot),
            artifacts: vec![],
            value,
            basis_revision,
            config_version: config.into(),
        }
    }
    pub fn with_artifact(mut self, artifact: ArtifactRef) -> Self {
        self.artifacts.push(artifact);
        self
    }
    pub fn hash(&self) -> String {
        digest(&json!(self))
    }
    pub fn output(&self) -> Value {
        self.value.clone()
    }
}
/// Only host registered implementations can issue evidence; no workspace executables loaded.
pub trait TargetValidator: Send + Sync {
    fn key(&self) -> ValidatorKey;
    fn check(&self, target: &Target) -> Result<Vec<Diagnostic>>;
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    pub id: String,
    pub target_hash: String,
    pub receipt: ValidationReceipt,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Source {
    System,
    Configuration,
    Input,
    Document,
    Tool,
    Summary,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContextItem {
    pub source: Source,
    pub value: Value,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ContextState {
    pub revision: u64,
    pub high_water: u64,
    pub purpose: String,
    pub constraints: Vec<String>,
    pub unfinished: Vec<String>,
    pub artifacts: Vec<ArtifactRef>,
    pub facts: Vec<ContextItem>,
    pub history: Vec<ContextItem>,
    pub summary: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct State {
    schema: u32,
    staged: BTreeMap<String, Staged>,
    revision: u64,
    artifact_revision: u64,
    files: BTreeMap<String, File>,
    grants: BTreeMap<String, Grant>,
    blobs: BTreeMap<String, Blob>,
    evidence: BTreeMap<String, Evidence>,
    completions: BTreeMap<String, String>,
    context: ContextState,
    retention: BTreeMap<DataClass, u64>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            schema: 2,
            staged: BTreeMap::new(),
            revision: 0,
            artifact_revision: 0,
            files: BTreeMap::new(),
            grants: BTreeMap::new(),
            blobs: BTreeMap::new(),
            evidence: BTreeMap::new(),
            completions: BTreeMap::new(),
            context: ContextState::default(),
            retention: BTreeMap::new(),
        }
    }
}

#[derive(Clone)]
enum Backend {
    Memory(Arc<Mutex<BTreeMap<String, Arc<Mutex<State>>>>>),
    Local(Arc<Local>),
    External(Arc<dyn NamespaceBackend>),
}
#[derive(Clone)]
pub struct DataStore {
    backend: Backend,
    policy: StoragePolicy,
}
/// Opaque, versioned namespace state. Adapters must durably CAS the complete
/// payload; no partial state may become visible. Version tokens must prevent ABA.
pub trait NamespaceBackend: Send + Sync {
    fn load(&self, namespace: &str) -> Result<(String, Option<Vec<u8>>)>;
    fn compare_exchange(&self, namespace: &str, expected: &str, payload: &[u8]) -> Result<()>;
}
impl DataStore {
    pub fn external(backend: Arc<dyn NamespaceBackend>, policy: StoragePolicy) -> Self {
        Self {
            backend: Backend::External(backend),
            policy,
        }
    }
    pub fn memory(policy: StoragePolicy) -> Self {
        Self {
            backend: Backend::Memory(Arc::default()),
            policy,
        }
    }
    pub fn local(root: &std::path::Path, policy: StoragePolicy) -> Result<Self> {
        Ok(Self {
            backend: Backend::Local(Arc::new(Local::open(root)?)),
            policy,
        })
    }
    pub fn policy(&self) -> &StoragePolicy {
        &self.policy
    }
    fn tx<R>(&self, a: &Access, write: bool, f: impl FnOnce(&mut State) -> Result<R>) -> Result<R> {
        a.actor.key()?;
        let key = a.target.key()?;
        let apply = |s: &mut State| {
            if s.schema != 2 {
                return Err(Error::Unsupported("data state schema".into()));
            }
            if a.actor != a.target {
                let g = a
                    .grant
                    .as_ref()
                    .and_then(|id| s.grants.get(id))
                    .ok_or(Error::PermissionDenied)?;
                if g.actor != a.actor
                    || g.revision != s.revision
                    || g.artifact_revision != s.artifact_revision
                    || g.context_revision != s.context.revision
                    || (write && !g.write)
                {
                    return Err(Error::PermissionDenied);
                }
            }
            for (path, file) in &s.files {
                logical_path(path)?;
                if file.version.path != *path
                    || file.version.hash != content_digest(&file.bytes)
                    || file.version.bytes != file.bytes.len() as u64
                    || file.version.revision > s.revision
                {
                    return Err(Error::Storage);
                }
            }
            let mut next = s.clone();
            let out = f(&mut next)?;
            *s = next;
            Ok(out)
        };
        match &self.backend {
            Backend::Memory(map) => {
                let state = map
                    .lock()
                    .map_err(|_| Error::Storage)?
                    .entry(key)
                    .or_default()
                    .clone();
                let mut guard = state.lock().map_err(|_| Error::Storage)?;
                apply(&mut guard)
            }
            Backend::Local(local) => local.tx(&key, apply),
            Backend::External(backend) => {
                let (version, bytes) = backend.load(&key)?;
                let mut state = match bytes {
                    Some(bytes) => serde_json::from_slice(&bytes).map_err(|_| Error::Storage)?,
                    None => State::default(),
                };
                let out = apply(&mut state)?;
                if write {
                    backend.compare_exchange(
                        &key,
                        &version,
                        &serde_json::to_vec(&state).map_err(|_| Error::Storage)?,
                    )?;
                }
                Ok(out)
            }
        }
    }
    fn class(&self, c: DataClass) -> Result<&ClassPolicy> {
        self.policy
            .classes
            .get(&c)
            .ok_or_else(|| invalid("CLASS_POLICY"))
    }
    fn snapshot_of(a: &Access, s: &State) -> WorkspaceSnapshot {
        let files = s
            .files
            .values()
            .map(|f| f.version.clone())
            .collect::<Vec<_>>();
        WorkspaceSnapshot {
            namespace: a.target.clone(),
            revision: s.revision,
            hash: digest(&json!([a.target, s.revision, files])),
            files,
        }
    }
    fn check_target(a: &Access, s: &State, target: &Target) -> Result<()> {
        if let Some(snapshot) = &target.snapshot
            && *snapshot != Self::snapshot_of(a, s)
        {
            return Err(Error::Conflict);
        }
        for r in &target.artifacts {
            let b = s.blobs.get(&r.id).ok_or(Error::NotFound)?;
            if r.owner != a.target || b.reference != *r || content_digest(&b.bytes) != r.hash {
                return Err(Error::Conflict);
            }
        }
        Ok(())
    }
    /// Host verifies the child record from RunStore; identity cannot be supplied by model.
    pub async fn share(
        &self,
        a: &Access,
        child: &RunRecord,
        agent: &str,
        write: bool,
        revision: u64,
    ) -> Result<Access> {
        let this = self.clone();
        let a = a.to_owned();
        let child = child.to_owned();
        let agent = agent.to_owned();
        tokio::task::spawn_blocking(move || {
            let this = &this;
            let a = &a;
            let child = &child;
            let agent = &agent;
            if a.actor != a.target
                || child.spec.parent.as_ref() != Some(&a.target.owner)
                || child.handle.tenant != a.target.owner.tenant
                || child.spec.tenant != child.handle.tenant
            {
                return Err(Error::PermissionDenied);
            }
            let actor = Namespace {
                owner: child.handle.clone(),
                agent: agent.into(),
            };
            actor.key()?;
            this.tx(a, true, |s| {
                if revision != s.revision {
                    return Err(Error::Conflict);
                }
                let id = uuid::Uuid::new_v4().to_string();
                if s.grants.len() >= this.policy.max_entries {
                    return Err(invalid("GRANT_CAPACITY"));
                }
                s.grants.insert(
                    id.clone(),
                    Grant {
                        actor: actor.clone(),
                        revision,
                        artifact_revision: s.artifact_revision,
                        context_revision: s.context.revision,
                        write,
                    },
                );
                Ok(Access {
                    actor,
                    target: a.target.clone(),
                    grant: Some(id),
                })
            })
        })
        .await
        .map_err(|_| Error::Storage)?
    }
    pub async fn revoke(&self, a: &Access) -> Result<()> {
        let this = self.clone();
        let a = a.to_owned();
        tokio::task::spawn_blocking(move || {
            let this = &this;
            let a = &a;
            if a.actor != a.target {
                return Err(Error::PermissionDenied);
            }
            this.tx(a, true, |s| {
                s.grants.clear();
                Ok(())
            })
        })
        .await
        .map_err(|_| Error::Storage)?
    }
    fn apply(
        &self,
        a: &Access,
        s: &mut State,
        expected: u64,
        changes: Vec<Change>,
    ) -> Result<WorkspaceSnapshot> {
        if s.revision != expected {
            return Err(Error::Conflict);
        }
        if changes.is_empty() || changes.len() > self.policy.max_edits {
            return Err(invalid("EDIT_LIMIT"));
        }
        let revision = s.revision.checked_add(1).ok_or(Error::Conflict)?;
        let mut seen = std::collections::BTreeSet::new();
        let mut input = 0usize;
        for change in changes {
            let path = match &change {
                Change::Write { path, .. } | Change::Delete { path, .. } => path,
            };
            logical_path(path)?;
            if !seen.insert(path.clone()) {
                return Err(invalid("DUPLICATE_PATH"));
            }
            match change {
                Change::Write {
                    path,
                    expected,
                    bytes,
                } => {
                    if s.files.get(&path).map(|f| f.version.revision) != expected {
                        return Err(Error::Conflict);
                    }
                    input = input
                        .checked_add(bytes.len())
                        .ok_or_else(|| invalid("CAPACITY"))?;
                    let version = FileVersion {
                        path: path.clone(),
                        revision,
                        hash: content_digest(&bytes),
                        bytes: bytes.len() as u64,
                    };
                    s.files.insert(path, File { version, bytes });
                }
                Change::Delete { path, expected } => {
                    if s.files.get(&path).map(|f| f.version.revision) != Some(expected) {
                        return Err(Error::Conflict);
                    }
                    s.files.remove(&path);
                }
            }
        }
        let limit = self.class(DataClass::Workspace)?.max_bytes;
        if input > limit
            || s.files.values().map(|f| f.bytes.len()).sum::<usize>() > limit
            || s.files.len() > self.policy.max_entries
        {
            return Err(invalid("CAPACITY"));
        }
        let mut aliases = BTreeMap::new();
        for path in s.files.keys() {
            let mut prefix = String::new();
            for part in path.split('/') {
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(part);
                if let Some(previous) = aliases.insert(prefix.to_ascii_lowercase(), prefix.clone())
                    && previous != prefix
                {
                    return Err(invalid("PATH_ALIAS"));
                }
            }
        }
        for path in s.files.keys() {
            let mut prefix = String::new();
            for part in path.split('/').take(path.split('/').count() - 1) {
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(part);
                if s.files.contains_key(&prefix) {
                    return Err(invalid("FILE_PARENT"));
                }
            }
        }
        s.revision = revision;
        Ok(Self::snapshot_of(a, s))
    }
    pub async fn commit(
        &self,
        a: &Access,
        expected: u64,
        changes: Vec<Change>,
    ) -> Result<WorkspaceSnapshot> {
        let this = self.clone();
        let a = a.to_owned();
        tokio::task::spawn_blocking(move || {
            let this = &this;
            let a = &a;
            this.tx(a, true, |s| this.apply(a, s, expected, changes))
        })
        .await
        .map_err(|_| Error::Storage)?
    }
    pub async fn exact_replace(
        &self,
        a: &Access,
        expected: u64,
        edits: Vec<ExactEdit>,
    ) -> Result<WorkspaceSnapshot> {
        let this = self.clone();
        let a = a.to_owned();
        tokio::task::spawn_blocking(move || {
            let this = &this;
            let a = &a;
            if edits.is_empty() || edits.len() > this.policy.max_edits {
                return Err(invalid("EDIT_LIMIT"));
            }
            this.tx(a, true, |s| {
                let mut total = 0usize;
                let mut changes: BTreeMap<String, (u64, Vec<u8>)> = BTreeMap::new();
                for e in edits {
                    logical_path(&e.path)?;
                    total = total
                        .checked_add(e.old_text.len())
                        .and_then(|n| n.checked_add(e.new_text.len()))
                        .ok_or_else(|| invalid("EDIT_LIMIT"))?;
                    if total > this.policy.max_edit_bytes {
                        return Err(invalid("EDIT_LIMIT"));
                    }
                    let file = s.files.get(&e.path).ok_or(Error::NotFound)?;
                    if file.version.hash != e.expected_digest {
                        return Err(Error::Conflict);
                    }
                    let staged_bytes = changes
                        .get(&e.path)
                        .map(|(_, bytes)| bytes.as_slice())
                        .unwrap_or(&file.bytes);
                    let text =
                        std::str::from_utf8(staged_bytes).map_err(|_| invalid("NON_UTF8"))?;
                    if binary_text(text) {
                        return Err(invalid("BINARY"));
                    }
                    if binary_text(&e.old_text) || binary_text(&e.new_text) {
                        return Err(invalid("BINARY"));
                    }
                    if e.old_text.is_empty() {
                        return Err(invalid("EMPTY_MATCH"));
                    }
                    // Overlapping occurrences count too, e.g. aa in aaa is ambiguous.
                    let count = text
                        .char_indices()
                        .filter(|(i, _)| text[*i..].starts_with(&e.old_text))
                        .count();
                    if count != 1 {
                        return Err(invalid(if count == 0 {
                            "NO_MATCH"
                        } else {
                            "MULTIPLE_MATCHES"
                        }));
                    }
                    let bytes = text.replacen(&e.old_text, &e.new_text, 1).into_bytes();
                    changes.insert(e.path, (file.version.revision, bytes));
                }
                this.apply(
                    a,
                    s,
                    expected,
                    changes
                        .into_iter()
                        .map(|(path, (revision, bytes))| Change::Write {
                            path,
                            expected: Some(revision),
                            bytes,
                        })
                        .collect(),
                )
            })
        })
        .await
        .map_err(|_| Error::Storage)?
    }
    pub fn stage(&self, a: &Access, bytes: &[u8], class: DataClass, now: u64) -> Result<Staged> {
        self.stage_named(a, bytes, class, now, uuid::Uuid::new_v4().to_string())
    }
    fn stage_named(
        &self,
        a: &Access,
        bytes: &[u8],
        class: DataClass,
        now: u64,
        id: String,
    ) -> Result<Staged> {
        if bytes.len() > self.class(class)?.max_bytes {
            return Err(invalid("CAPACITY"));
        }
        let staged = Staged {
            id,
            owner: a.target.clone(),
            bytes: bytes.to_vec(),
            class,
            expires_at: now.saturating_add(self.class(class)?.retention),
        };
        self.tx(a, true, |s| {
            if let Some(blob) = s.blobs.get(&staged.id) {
                if blob.bytes != staged.bytes || blob.reference.class != class {
                    return Err(Error::Conflict);
                }
                return Ok(staged);
            }
            if let Some(previous) = s.staged.get(&staged.id) {
                if previous.bytes != staged.bytes || previous.class != class {
                    return Err(Error::Conflict);
                }
                return Ok(previous.clone());
            }
            if s.staged.len() >= self.policy.max_entries
                || s.staged
                    .values()
                    .filter(|v| v.class == class)
                    .map(|v| v.bytes.len())
                    .sum::<usize>()
                    .saturating_add(bytes.len())
                    > self.class(class)?.max_bytes
            {
                return Err(invalid("STAGING_CAPACITY"));
            }
            s.staged.insert(staged.id.clone(), staged.clone());
            Ok(staged)
        })
    }
    /// Resume a persisted staging operation after worker loss.
    pub fn staged(&self, a: &Access, id: &str) -> Result<Staged> {
        if a.actor != a.target {
            return Err(Error::PermissionDenied);
        }
        self.tx(a, false, |s| {
            s.staged.get(id).cloned().ok_or(Error::NotFound)
        })
    }
    /// Only the owning namespace can reclaim expired, unpublished staging data.
    pub fn cleanup_staging(&self, a: &Access, now: u64) -> Result<usize> {
        if a.actor != a.target {
            return Err(Error::PermissionDenied);
        }
        self.tx(a, true, |s| {
            let before = s.staged.len();
            s.staged.retain(|_, value| now < value.expires_at);
            Ok(before - s.staged.len())
        })
    }
    pub async fn save(&self, a: &Access, staged: Staged, now: u64) -> Result<ArtifactRef> {
        let this = self.clone();
        let a = a.to_owned();
        tokio::task::spawn_blocking(move || {
            let this = &this;
            let a = &a;
            if staged.owner != a.target {
                return Err(Error::PermissionDenied);
            }
            this.tx(a, true, |s| {
                if let Some(existing) = s.blobs.get(&staged.id) {
                    if existing.bytes != staged.bytes || existing.reference.class != staged.class {
                        return Err(Error::Conflict);
                    }
                    return Ok(existing.reference.clone());
                }
                if s.staged.get(&staged.id) != Some(&staged) {
                    return Err(Error::Conflict);
                }
                let policy = this.class(staged.class)?;
                if s.blobs
                    .values()
                    .filter(|b| b.reference.class == staged.class)
                    .map(|b| b.bytes.len())
                    .sum::<usize>()
                    .saturating_add(staged.bytes.len())
                    > policy.max_bytes
                    || s.blobs.len() >= this.policy.max_entries
                {
                    return Err(invalid("CAPACITY"));
                }
                s.artifact_revision = s.artifact_revision.checked_add(1).ok_or(Error::Conflict)?;
                let r = ArtifactRef {
                    owner: a.target.clone(),
                    id: staged.id,
                    revision: s.artifact_revision,
                    hash: content_digest(&staged.bytes),
                    bytes: staged.bytes.len() as u64,
                    manifest: Self::snapshot_of(a, s).files,
                    retention_until: now.saturating_add(policy.retention),
                    class: staged.class,
                };
                s.staged.remove(&r.id);
                s.blobs.insert(
                    r.id.clone(),
                    Blob {
                        reference: r.clone(),
                        bytes: staged.bytes,
                        completion: None,
                    },
                );
                Ok(r)
            })
        })
        .await
        .map_err(|_| Error::Storage)?
    }
    pub async fn artifact(&self, a: &Access, id: &str) -> Result<ArtifactRef> {
        let this = self.clone();
        let a = a.to_owned();
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || {
            let this = &this;
            let a = &a;
            let id = &id;
            this.tx(a, false, |s| {
                Ok(s.blobs.get(id).ok_or(Error::NotFound)?.reference.clone())
            })
        })
        .await
        .map_err(|_| Error::Storage)?
    }
    pub async fn validate<V: TargetValidator + 'static>(
        &self,
        a: &Access,
        target: &Target,
        validator: Arc<V>,
    ) -> Result<Evidence> {
        let store = self.clone();
        let a = a.clone();
        let target = target.clone();
        tokio::task::spawn_blocking(move || store.validate_sync(&a, &target, validator.as_ref()))
            .await
            .map_err(|_| Error::Storage)?
    }
    fn validate_sync(
        &self,
        a: &Access,
        target: &Target,
        validator: &impl TargetValidator,
    ) -> Result<Evidence> {
        self.tx(a, false, |s| Self::check_target(a, s, target))?;
        let diagnostics = validator.check(target)?;
        let key = validator.key();
        let receipt = ValidationReceipt {
            hash: digest(&target.output()),
            basis_revision: target.basis_revision,
            config_version: target.config_version.clone(),
            validator: key.name,
            validator_version: key.version,
            passed: !diagnostics
                .iter()
                .any(|d| d.severity != "info" && d.severity != "warning"),
            diagnostics,
        };
        let evidence = Evidence {
            id: uuid::Uuid::new_v4().to_string(),
            target_hash: target.hash(),
            receipt,
        };
        self.tx(a, true, |s| {
            Self::check_target(a, s, target)?;
            if s.evidence.len() >= self.policy.max_entries {
                return Err(invalid("EVIDENCE_CAPACITY"));
            }
            s.evidence.insert(evidence.id.clone(), evidence.clone());
            Ok(evidence)
        })
    }
    /// Current target includes current basis/config supplied by the trusted owner, not Agent.
    pub async fn finalize_target(
        &self,
        a: &Access,
        current: &Target,
        evidence: &[Evidence],
        required: &[ValidatorKey],
    ) -> Result<String> {
        let this = self.clone();
        let a = a.to_owned();
        let current = current.to_owned();
        let evidence = evidence.to_owned();
        let required = required.to_owned();
        tokio::task::spawn_blocking(move || {
            let this = &this;
            let a = &a;
            let current = &current;
            let evidence = &evidence;
            let required = &required;
            this.finalize_sync(a, current, evidence, required)
        })
        .await
        .map_err(|_| Error::Storage)?
    }
    fn finalize_sync(
        &self,
        a: &Access,
        current: &Target,
        evidence: &[Evidence],
        required: &[ValidatorKey],
    ) -> Result<String> {
        if required.is_empty() {
            return Err(invalid("VALIDATORS_REQUIRED"));
        }
        self.tx(a, true, |s| {
            Self::check_target(a, s, current)?;
            for key in required {
                if !evidence.iter().any(|e| {
                    s.evidence.get(&e.id) == Some(e)
                        && e.target_hash == current.hash()
                        && e.receipt.passed
                        && e.receipt.hash == digest(&current.output())
                        && e.receipt.basis_revision == current.basis_revision
                        && e.receipt.config_version == current.config_version
                        && e.receipt.validator == key.name
                        && e.receipt.validator_version == key.version
                }) {
                    return Err(Error::Conflict);
                }
            }
            let key = digest(&json!([current, required]));
            if !s.completions.contains_key(&key) && s.completions.len() >= self.policy.max_entries {
                return Err(invalid("COMPLETION_CAPACITY"));
            }
            let id = s
                .completions
                .entry(key)
                .or_insert_with(|| uuid::Uuid::new_v4().to_string())
                .clone();
            let final_bytes = s
                .blobs
                .values()
                .filter(|b| {
                    b.completion.is_some()
                        || current.artifacts.iter().any(|r| r.id == b.reference.id)
                })
                .map(|b| b.bytes.len())
                .sum::<usize>();
            if final_bytes > self.class(DataClass::Final)?.max_bytes {
                return Err(invalid("FINAL_CAPACITY"));
            }
            for r in &current.artifacts {
                let blob = s.blobs.get_mut(&r.id).ok_or(Error::NotFound)?;
                if blob
                    .completion
                    .as_ref()
                    .is_some_and(|previous| previous != &id)
                {
                    return Err(Error::Conflict);
                }
                blob.completion = Some(id.clone());
            }
            Ok(id)
        })
    }
    pub async fn context(&self, a: &Access) -> Result<ContextState> {
        let this = self.clone();
        let a = a.to_owned();
        tokio::task::spawn_blocking(move || {
            let this = &this;
            let a = &a;
            this.tx(a, false, |s| Ok(s.context.clone()))
        })
        .await
        .map_err(|_| Error::Storage)?
    }
    pub async fn append_context(&self, a: &Access, item: ContextItem) -> Result<ContextState> {
        let this = self.clone();
        let a = a.to_owned();
        tokio::task::spawn_blocking(move || {
            let this = &this;
            let a = &a;
            this.tx(a, true, |s| {
                let mut next = s.context.clone();
                next.high_water += 1;
                next.revision += 1;
                next.facts.push(item.clone());
                next.history.push(item);
                if serde_json::to_vec(&next).map_err(|_| Error::Storage)?.len()
                    > this.class(DataClass::Checkpoint)?.max_bytes
                {
                    return Err(invalid("CONTEXT_CAPACITY"));
                }
                s.context = next.clone();
                Ok(next)
            })
        })
        .await
        .map_err(|_| Error::Storage)?
    }
    pub async fn commit_compaction(
        &self,
        a: &Access,
        base: &ContextState,
        summary: &str,
        complete: bool,
    ) -> Result<ContextState> {
        let this = self.clone();
        let a = a.to_owned();
        let base = base.to_owned();
        let summary = summary.to_owned();
        tokio::task::spawn_blocking(move || {
            let this = &this;
            let a = &a;
            let base = &base;
            let summary = &summary;
            if !complete || summary.is_empty() {
                return Err(invalid("PARTIAL_SUMMARY"));
            }
            this.tx(a, true, |s| {
                if &s.context != base {
                    return Err(Error::Conflict);
                }
                let mut next = base.clone();
                next.revision += 1;
                next.summary = Some(summary.into());
                next.history = vec![ContextItem {
                    source: Source::Summary,
                    value: json!(summary),
                }];
                if serde_json::to_vec(&next).map_err(|_| Error::Storage)?.len()
                    > this.class(DataClass::Checkpoint)?.max_bytes
                {
                    return Err(invalid("CONTEXT_CAPACITY"));
                }
                s.context = next.clone();
                Ok(next)
            })
        })
        .await
        .map_err(|_| Error::Storage)?
    }
}
pub trait WorkspaceStore: Send + Sync {
    fn snapshot(
        &self,
        a: &Access,
    ) -> impl std::future::Future<Output = Result<WorkspaceSnapshot>> + Send;
    fn list(
        &self,
        a: &Access,
        cursor: Option<String>,
        limit: usize,
    ) -> impl std::future::Future<Output = Result<Page<FileVersion>>> + Send;
    fn read(
        &self,
        a: &Access,
        path: &str,
        revision: u64,
        offset: u64,
        length: usize,
    ) -> impl std::future::Future<Output = Result<ByteRange>> + Send;
    fn write(
        &self,
        a: &Access,
        path: &str,
        expected: Option<u64>,
        bytes: &[u8],
    ) -> impl std::future::Future<Output = Result<FileVersion>> + Send;
    fn delete(
        &self,
        a: &Access,
        path: &str,
        expected: u64,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
    fn search(
        &self,
        a: &Access,
        query: &str,
        cursor: Option<String>,
        limit: usize,
        max_bytes: usize,
    ) -> impl std::future::Future<Output = Result<Page<Match>>> + Send;
    fn merge(
        &self,
        a: &Access,
        base: &WorkspaceSnapshot,
        changes: Vec<Change>,
    ) -> impl std::future::Future<Output = Result<WorkspaceSnapshot>> + Send;
}
impl WorkspaceStore for DataStore {
    async fn snapshot(&self, a: &Access) -> Result<WorkspaceSnapshot> {
        let this = self.clone();
        let a = a.to_owned();
        tokio::task::spawn_blocking(move || {
            let this = &this;
            let a = &a;
            this.tx(a, false, |s| Ok(Self::snapshot_of(a, s)))
        })
        .await
        .map_err(|_| Error::Storage)?
    }
    async fn list(
        &self,
        a: &Access,
        cursor: Option<String>,
        limit: usize,
    ) -> Result<Page<FileVersion>> {
        let this = self.clone();
        let a = a.to_owned();
        tokio::task::spawn_blocking(move || {
            let this = &this;
            let a = &a;
            if limit == 0 || limit > this.policy.max_entries {
                return Err(invalid("PAGE_LIMIT"));
            }
            this.tx(a, false, |s| {
                let start = decode_cursor(cursor, s.revision)?;
                let entries = s
                    .files
                    .values()
                    .skip(start)
                    .take(limit)
                    .map(|f| f.version.clone())
                    .collect::<Vec<_>>();
                let end = start + entries.len();
                let truncated = end < s.files.len();
                if start > s.files.len() {
                    return Err(invalid("CURSOR"));
                }
                Ok(Page {
                    entries,
                    next: truncated.then(|| format!("{}:{end}", s.revision)),
                    truncated,
                })
            })
        })
        .await
        .map_err(|_| Error::Storage)?
    }
    async fn read(
        &self,
        a: &Access,
        path: &str,
        revision: u64,
        offset: u64,
        length: usize,
    ) -> Result<ByteRange> {
        let this = self.clone();
        let a = a.to_owned();
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || {
            let this = &this;
            let a = &a;
            let path = &path;
            logical_path(path)?;
            this.tx(a, false, |s| {
                let f = s.files.get(path).ok_or(Error::NotFound)?;
                if f.version.revision != revision {
                    return Err(Error::Conflict);
                }
                range(&f.bytes, offset, length, this.policy.max_read)
            })
        })
        .await
        .map_err(|_| Error::Storage)?
    }
    async fn write(
        &self,
        a: &Access,
        path: &str,
        expected: Option<u64>,
        bytes: &[u8],
    ) -> Result<FileVersion> {
        let this = self.clone();
        let a = a.to_owned();
        let path = path.to_owned();
        let bytes = bytes.to_owned();
        tokio::task::spawn_blocking(move || {
            let this = &this;
            let a = &a;
            let path = &path;
            let bytes = &bytes;
            this.tx(a, true, |s| {
                let snap = this.apply(
                    a,
                    s,
                    s.revision,
                    vec![Change::Write {
                        path: path.into(),
                        expected,
                        bytes: bytes.to_vec(),
                    }],
                )?;
                snap.files
                    .into_iter()
                    .find(|f| f.path == path.as_str())
                    .ok_or(Error::Storage)
            })
        })
        .await
        .map_err(|_| Error::Storage)?
    }
    async fn delete(&self, a: &Access, path: &str, expected: u64) -> Result<()> {
        let this = self.clone();
        let a = a.to_owned();
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || {
            let this = &this;
            let a = &a;
            let path = &path;
            this.tx(a, true, |s| {
                this.apply(
                    a,
                    s,
                    s.revision,
                    vec![Change::Delete {
                        path: path.into(),
                        expected,
                    }],
                )?;
                Ok(())
            })
        })
        .await
        .map_err(|_| Error::Storage)?
    }
    async fn search(
        &self,
        a: &Access,
        query: &str,
        cursor: Option<String>,
        limit: usize,
        max_bytes: usize,
    ) -> Result<Page<Match>> {
        let this = self.clone();
        let a = a.to_owned();
        let query = query.to_owned();
        tokio::task::spawn_blocking(move || {
            let this = &this;
            let a = &a;
            let query = &query;
            if query.is_empty()
                || query.len() > 4096
                || limit == 0
                || limit > this.policy.max_entries
                || max_bytes < 128
                || max_bytes > this.policy.max_read
            {
                return Err(invalid("SEARCH_LIMIT"));
            }
            this.tx(a, false, |s| {
                let start = decode_cursor(cursor, s.revision)?;
                let mut index = 0;
                let mut entries = Vec::new();
                let mut truncated = false;
                let mut next_index = start;
                'files: for (path, file) in &s.files {
                    let Ok(text) = std::str::from_utf8(&file.bytes) else {
                        continue;
                    };
                    if text.contains('\0') {
                        continue;
                    }
                    for (line, text) in text.lines().enumerate() {
                        if !text.contains(query) {
                            continue;
                        }
                        if index < start {
                            index += 1;
                            continue;
                        }
                        let m = Match {
                            path: path.clone(),
                            line: line + 1,
                            text: text.into(),
                        };
                        let mut trial = entries.clone();
                        trial.push(m.clone());
                        let trial_page = Page {
                            entries: trial,
                            next: Some(format!("{}:{}", s.revision, index + 1)),
                            truncated: true,
                        };
                        if entries.len() == limit
                            || serde_json::to_vec(&trial_page)
                                .map_err(|_| Error::Storage)?
                                .len()
                                > max_bytes
                        {
                            if entries.is_empty() {
                                return Err(invalid("SEARCH_MATCH_TOO_LARGE"));
                            }
                            truncated = true;
                            break 'files;
                        }
                        entries.push(m);
                        index += 1;
                        next_index = index;
                    }
                }
                Ok(Page {
                    entries,
                    next: truncated.then(|| format!("{}:{next_index}", s.revision)),
                    truncated,
                })
            })
        })
        .await
        .map_err(|_| Error::Storage)?
    }
    async fn merge(
        &self,
        a: &Access,
        base: &WorkspaceSnapshot,
        changes: Vec<Change>,
    ) -> Result<WorkspaceSnapshot> {
        let this = self.clone();
        let a = a.to_owned();
        let base = base.to_owned();
        tokio::task::spawn_blocking(move || {
            let this = &this;
            let a = &a;
            let base = &base;
            this.tx(a, true, |s| {
                if *base != Self::snapshot_of(a, s) {
                    return Err(Error::Conflict);
                }
                this.apply(a, s, base.revision, changes)
            })
        })
        .await
        .map_err(|_| Error::Storage)?
    }
}
fn decode_cursor(cursor: Option<String>, revision: u64) -> Result<usize> {
    match cursor {
        None => Ok(0),
        Some(c) => {
            let (r, n) = c.split_once(':').ok_or_else(|| invalid("CURSOR"))?;
            if r.parse::<u64>().ok() != Some(revision) {
                return Err(Error::Conflict);
            }
            n.parse().map_err(|_| invalid("CURSOR"))
        }
    }
}
pub trait ArtifactStore: Send + Sync {
    fn put(
        &self,
        a: &Access,
        bytes: &[u8],
        class: DataClass,
        now: u64,
    ) -> impl std::future::Future<Output = Result<ArtifactRef>> + Send;
    fn read_artifact(
        &self,
        a: &Access,
        artifact: &ArtifactRef,
        offset: u64,
        length: usize,
    ) -> impl std::future::Future<Output = Result<ByteRange>> + Send;
    fn delete_artifact(
        &self,
        a: &Access,
        artifact: &ArtifactRef,
        now: u64,
    ) -> impl std::future::Future<Output = Result<Progress>> + Send;
}
impl ArtifactStore for DataStore {
    async fn put(
        &self,
        a: &Access,
        bytes: &[u8],
        class: DataClass,
        now: u64,
    ) -> Result<ArtifactRef> {
        let this = self.clone();
        let a = a.to_owned();
        let bytes = bytes.to_owned();
        let access = a.clone();
        let staged = tokio::task::spawn_blocking(move || this.stage(&access, &bytes, class, now))
            .await
            .map_err(|_| Error::Storage)??;
        self.save(&a, staged, now).await
    }
    async fn read_artifact(
        &self,
        a: &Access,
        r: &ArtifactRef,
        offset: u64,
        length: usize,
    ) -> Result<ByteRange> {
        let this = self.clone();
        let a = a.to_owned();
        let r = r.to_owned();
        tokio::task::spawn_blocking(move || {
            let this = &this;
            let a = &a;
            let r = &r;
            if r.owner != a.target {
                return Err(Error::PermissionDenied);
            }
            this.tx(a, false, |s| {
                let b = s.blobs.get(&r.id).ok_or(Error::NotFound)?;
                if b.reference != *r || content_digest(&b.bytes) != r.hash {
                    return Err(Error::Conflict);
                }
                range(&b.bytes, offset, length, this.policy.max_read)
            })
        })
        .await
        .map_err(|_| Error::Storage)?
    }
    async fn delete_artifact(&self, a: &Access, r: &ArtifactRef, now: u64) -> Result<Progress> {
        let this = self.clone();
        let a = a.to_owned();
        let r = r.to_owned();
        tokio::task::spawn_blocking(move || {
            let this = &this;
            let a = &a;
            let r = &r;
            this.tx(a, true, |s| {
                if r.owner != a.target {
                    return Err(Error::PermissionDenied);
                }
                let p = this.class(r.class)?;
                if !p.deletable || now < r.retention_until {
                    return Err(Error::PermissionDenied);
                }
                if let Some(b) = s.blobs.get(&r.id) {
                    if b.completion.is_some()
                        && (!this.class(DataClass::Final)?.deletable
                            || now
                                < r.retention_until
                                    .saturating_add(this.class(DataClass::Final)?.retention))
                    {
                        return Err(Error::PermissionDenied);
                    }
                    if b.reference != *r {
                        return Err(Error::Conflict);
                    }
                    s.blobs.remove(&r.id);
                }
                Ok(Progress::Confirmed)
            })
        })
        .await
        .map_err(|_| Error::Storage)?
    }
}
impl DataStore {
    /// Host supplied purpose and constraints remain separate from model summaries.
    pub async fn context_contract(
        &self,
        a: &Access,
        expected: u64,
        purpose: String,
        constraints: Vec<String>,
        unfinished: Vec<String>,
        artifacts: Vec<ArtifactRef>,
    ) -> Result<ContextState> {
        let this = self.clone();
        let a = a.to_owned();
        tokio::task::spawn_blocking(move || {
            let this = &this;
            let a = &a;
            this.tx(a, true, |s| {
                if s.context.revision != expected {
                    return Err(Error::Conflict);
                }
                for r in &artifacts {
                    if r.owner != a.target || s.blobs.get(&r.id).map(|b| &b.reference) != Some(r) {
                        return Err(Error::PermissionDenied);
                    }
                }
                let mut next = s.context.clone();
                next.revision += 1;
                next.purpose = purpose;
                next.constraints = constraints;
                next.unfinished = unfinished;
                next.artifacts = artifacts;
                if serde_json::to_vec(&next).map_err(|_| Error::Storage)?.len()
                    > this.class(DataClass::Checkpoint)?.max_bytes
                {
                    return Err(invalid("CONTEXT_CAPACITY"));
                }
                s.context = next.clone();
                Ok(next)
            })
        })
        .await
        .map_err(|_| Error::Storage)?
    }
    /// Establish a monotonic retention boundary. Host supplies the trusted clock.
    pub async fn retain(&self, a: &Access, class: DataClass, now: u64) -> Result<u64> {
        let this = self.clone();
        let a = a.to_owned();
        tokio::task::spawn_blocking(move || {
            let this = &this;
            let a = &a;
            this.tx(a, true, |s| {
                let until = now.saturating_add(this.class(class)?.retention);
                let r = s.retention.entry(class).or_default();
                *r = (*r).max(until);
                Ok(*r)
            })
        })
        .await
        .map_err(|_| Error::Storage)?
    }
    /// Explicit cleanup, never an inference from Run termination or a timeout.
    pub async fn cleanup_class(
        &self,
        a: &Access,
        class: DataClass,
        expected: u64,
        now: u64,
    ) -> Result<Progress> {
        let this = self.clone();
        let a = a.to_owned();
        tokio::task::spawn_blocking(move || {
            let this = &this;
            let a = &a;
            if a.actor != a.target {
                return Err(Error::PermissionDenied);
            }
            this.tx(a, true, |s| {
                if s.revision != expected {
                    return Err(Error::Conflict);
                }
                if !this.class(class)?.deletable || s.retention.get(&class).is_none_or(|n| now < *n)
                {
                    return Err(Error::PermissionDenied);
                }
                match class {
                    DataClass::Workspace => {
                        s.files.clear();
                        s.grants.clear();
                        s.revision += 1;
                    }
                    DataClass::Checkpoint => {
                        s.context = ContextState {
                            revision: s.context.revision + 1,
                            ..ContextState::default()
                        };
                    }
                    _ => {}
                }
                s.blobs.retain(|_, b| {
                    b.reference.class != class
                        || now < b.reference.retention_until
                        || b.completion.is_some()
                });
                s.staged
                    .retain(|_, staged| staged.class != class || now < staged.expires_at);
                Ok(
                    if s.blobs.values().any(|b| b.reference.class == class)
                        || s.staged.values().any(|staged| staged.class == class)
                    {
                        Progress::Pending
                    } else {
                        Progress::Confirmed
                    },
                )
            })
        })
        .await
        .map_err(|_| Error::Storage)?
    }
}
/// Driver adapters use exactly the same evidence and finalization contract for
/// structured decisions and file-backed results. References are host-bound.
pub struct DataValidator<V, S = DataStore> {
    pub store: S,
    pub access: Access,
    pub validator: V,
    pub workspace: bool,
    pub artifacts: Vec<ArtifactRef>,
}
pub struct DataFinalizer<S = DataStore> {
    pub store: S,
    pub access: Access,
    pub required: Vec<ValidatorKey>,
    pub workspace: bool,
    pub artifacts: Vec<ArtifactRef>,
}
impl DataStore {
    fn run_target(
        &self,
        a: &Access,
        r: &RunRecord,
        value: Value,
        workspace: bool,
        artifacts: Vec<ArtifactRef>,
    ) -> Result<Target> {
        if a.actor.owner != r.handle {
            return Err(Error::PermissionDenied);
        }
        let snapshot = if workspace {
            Some(self.tx(a, false, |s| Ok(Self::snapshot_of(a, s)))?)
        } else {
            None
        };
        Ok(Target {
            snapshot,
            artifacts,
            value,
            basis_revision: r.spec.basis_revision,
            config_version: r.spec.config_version.clone(),
        })
    }
}
impl<V: TargetValidator, S: EvidenceStore> Validator for DataValidator<V, S> {
    fn validate(&self, run: &RunRecord, candidate: &Value) -> Result<ValidationReceipt> {
        let target = self.store.run_target(
            &self.access,
            run,
            candidate.clone(),
            self.workspace,
            self.artifacts.clone(),
        )?;
        Ok(self
            .store
            .validate_sync(&self.access, &target, &self.validator)?
            .receipt)
    }
}
impl<S: EvidenceStore> Finalizer for DataFinalizer<S> {
    fn finalize(
        &self,
        run: &RunRecord,
        candidate: Value,
        receipt: ValidationReceipt,
    ) -> Result<Completion> {
        if !receipt.passed {
            return Err(invalid("VALIDATION_FAILED"));
        }
        let target = self.store.run_target(
            &self.access,
            run,
            candidate.clone(),
            self.workspace,
            self.artifacts.clone(),
        )?;
        let evidence = self.store.evidence(&self.access, &target)?;
        if !evidence.iter().any(|e| e.receipt == receipt) {
            return Err(Error::Conflict);
        }
        let id = self
            .store
            .finalize_sync(&self.access, &target, &evidence, &self.required)?;
        Ok(Completion {
            id,
            output: candidate,
            receipt,
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArtifactStatus {
    Saved,
    Finalized { completion_id: String },
}
impl DataStore {
    pub async fn artifact_status(&self, a: &Access, r: &ArtifactRef) -> Result<ArtifactStatus> {
        let this = self.clone();
        let a = a.to_owned();
        let r = r.to_owned();
        tokio::task::spawn_blocking(move || {
            let this = &this;
            let a = &a;
            let r = &r;
            this.tx(a, false, |s| {
                if r.owner != a.target {
                    return Err(Error::PermissionDenied);
                }
                let b = s.blobs.get(&r.id).ok_or(Error::NotFound)?;
                if b.reference != *r {
                    return Err(Error::Conflict);
                }
                Ok(match &b.completion {
                    None => ArtifactStatus::Saved,
                    Some(id) => ArtifactStatus::Finalized {
                        completion_id: id.clone(),
                    },
                })
            })
        })
        .await
        .map_err(|_| Error::Storage)?
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum StoredOutput {
    Inline(Value),
    External {
        artifact: ArtifactRef,
        preview: ByteRange,
    },
}
impl DataStore {
    /// Stable per-operation external output, so recovery does not create new blobs.
    pub async fn store_output(
        &self,
        a: &Access,
        operation: &str,
        value: Value,
        inline_limit: usize,
        now: u64,
    ) -> Result<StoredOutput> {
        if inline_limit > self.policy.max_read || inline_limit == 0 {
            return Err(invalid("OUTPUT_LIMIT"));
        }
        let bytes = serde_json::to_vec(&value).map_err(|_| Error::Storage)?;
        if bytes.len() <= inline_limit {
            return Ok(StoredOutput::Inline(value));
        }
        let store = self.clone();
        let access = a.clone();
        let id = digest(&json!([a.target, operation, "output"]));
        let staged = tokio::task::spawn_blocking(move || {
            store.stage_named(&access, &bytes, DataClass::Intermediate, now, id)
        })
        .await
        .map_err(|_| Error::Storage)??;
        let artifact = self.save(a, staged, now).await?;
        let preview = self.read_artifact(a, &artifact, 0, inline_limit).await?;
        Ok(StoredOutput::External { artifact, preview })
    }
}
impl DataStore {
    /// Explicit sharing with a child Agent in the same Run. Only namespace owner
    /// can issue this capability; a model cannot select a different actor.
    pub async fn share_agent(
        &self,
        a: &Access,
        child_agent: &str,
        write: bool,
        revision: u64,
    ) -> Result<Access> {
        let this = self.clone();
        let a = a.to_owned();
        let child_agent = child_agent.to_owned();
        tokio::task::spawn_blocking(move || {
            let this = &this;
            let a = &a;
            let child_agent = &child_agent;
            if a.actor != a.target || child_agent.as_str() == a.target.agent {
                return Err(Error::PermissionDenied);
            }
            let actor = Namespace {
                owner: a.target.owner.clone(),
                agent: child_agent.into(),
            };
            actor.key()?;
            this.tx(a, true, |s| {
                if s.revision != revision {
                    return Err(Error::Conflict);
                }
                let id = uuid::Uuid::new_v4().to_string();
                if s.grants.len() >= this.policy.max_entries {
                    return Err(invalid("GRANT_CAPACITY"));
                }
                s.grants.insert(
                    id.clone(),
                    Grant {
                        actor: actor.clone(),
                        revision,
                        artifact_revision: s.artifact_revision,
                        context_revision: s.context.revision,
                        write,
                    },
                );
                Ok(Access {
                    actor,
                    target: a.target.clone(),
                    grant: Some(id),
                })
            })
        })
        .await
        .map_err(|_| Error::Storage)?
    }
}

/// Synchronous evidence boundary. Runtime and ValidationTool offload it via spawn_blocking.
/// Direct callers must supply the same boundary for blocking/network implementations.
/// Implementations must compare current target and persisted evidence atomically
/// when publishing the completion marker.
pub trait EvidenceStore: Send + Sync {
    fn run_target(
        &self,
        a: &Access,
        run: &RunRecord,
        value: Value,
        workspace: bool,
        artifacts: Vec<ArtifactRef>,
    ) -> Result<Target>;
    fn validate_sync(
        &self,
        a: &Access,
        target: &Target,
        validator: &impl TargetValidator,
    ) -> Result<Evidence>;
    fn evidence(&self, a: &Access, target: &Target) -> Result<Vec<Evidence>>;
    fn finalize_sync(
        &self,
        a: &Access,
        current: &Target,
        evidence: &[Evidence],
        required: &[ValidatorKey],
    ) -> Result<String>;
}
impl EvidenceStore for DataStore {
    fn run_target(
        &self,
        a: &Access,
        run: &RunRecord,
        value: Value,
        workspace: bool,
        artifacts: Vec<ArtifactRef>,
    ) -> Result<Target> {
        DataStore::run_target(self, a, run, value, workspace, artifacts)
    }
    fn validate_sync(
        &self,
        a: &Access,
        target: &Target,
        validator: &impl TargetValidator,
    ) -> Result<Evidence> {
        DataStore::validate_sync(self, a, target, validator)
    }
    fn evidence(&self, a: &Access, target: &Target) -> Result<Vec<Evidence>> {
        self.tx(a, false, |s| {
            Ok(s.evidence
                .values()
                .filter(|e| e.target_hash == target.hash())
                .cloned()
                .collect())
        })
    }
    fn finalize_sync(
        &self,
        a: &Access,
        current: &Target,
        evidence: &[Evidence],
        required: &[ValidatorKey],
    ) -> Result<String> {
        DataStore::finalize_sync(self, a, current, evidence, required)
    }
}
/// Additional standard tool operations; adapters may implement these directly
/// or use DataStore with a NamespaceBackend to reuse the reference algorithms.
pub trait StandardToolStore: WorkspaceStore + ArtifactStore {
    fn policy(&self) -> &StoragePolicy;
    fn exact_replace(
        &self,
        a: &Access,
        expected: u64,
        edits: Vec<ExactEdit>,
    ) -> impl std::future::Future<Output = Result<WorkspaceSnapshot>> + Send;
    fn store_output(
        &self,
        a: &Access,
        operation: &str,
        value: Value,
        inline_limit: usize,
        now: u64,
    ) -> impl std::future::Future<Output = Result<StoredOutput>> + Send;
}
impl StandardToolStore for DataStore {
    fn policy(&self) -> &StoragePolicy {
        DataStore::policy(self)
    }
    async fn exact_replace(
        &self,
        a: &Access,
        expected: u64,
        edits: Vec<ExactEdit>,
    ) -> Result<WorkspaceSnapshot> {
        DataStore::exact_replace(self, a, expected, edits).await
    }
    async fn store_output(
        &self,
        a: &Access,
        operation: &str,
        value: Value,
        inline_limit: usize,
        now: u64,
    ) -> Result<StoredOutput> {
        DataStore::store_output(self, a, operation, value, inline_limit, now).await
    }
}

/// Context is a separate CAS boundary from workspace and artifact publication.
pub trait ContextStore: Send + Sync {
    fn context(
        &self,
        access: &Access,
    ) -> impl std::future::Future<Output = Result<ContextState>> + Send;
    fn append_context(
        &self,
        access: &Access,
        item: ContextItem,
    ) -> impl std::future::Future<Output = Result<ContextState>> + Send;
    fn commit_compaction(
        &self,
        access: &Access,
        base: &ContextState,
        summary: &str,
        complete: bool,
    ) -> impl std::future::Future<Output = Result<ContextState>> + Send;
}
impl ContextStore for DataStore {
    async fn context(&self, access: &Access) -> Result<ContextState> {
        DataStore::context(self, access).await
    }
    async fn append_context(&self, access: &Access, item: ContextItem) -> Result<ContextState> {
        DataStore::append_context(self, access, item).await
    }
    async fn commit_compaction(
        &self,
        access: &Access,
        base: &ContextState,
        summary: &str,
        complete: bool,
    ) -> Result<ContextState> {
        DataStore::commit_compaction(self, access, base, summary, complete).await
    }
}

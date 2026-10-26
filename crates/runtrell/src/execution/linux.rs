// SPDX-License-Identifier: Apache-2.0
//! cgroup v2 + bubblewrap + host-mounted bounded tmpfs. No process registry is needed.
//! Control files are outside the sandbox. A durable Running record is never restarted.
use super::process::{Entry, bounded, collect_files};
use super::*;
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::process::Command;

/// Host-selected cgroup CPU bandwidth, separate from the per-request cumulative threshold.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinuxCpuBandwidth {
    pub quota_usec: u64,
    pub period_usec: u64,
}
impl LinuxCpuBandwidth {
    fn validate(&self) -> Result<()> {
        if self.quota_usec < 1000 || !(1000..=1_000_000).contains(&self.period_usec) {
            return Err(Error::Invalid("LINUX_CPU_BANDWIDTH".into()));
        }
        Ok(())
    }
}
#[derive(Clone)]
pub struct LinuxBackend {
    root: PathBuf,
    bandwidth: LinuxCpuBandwidth,
    cgroups: PathBuf,
    helper: PathBuf,
    launcher: PathBuf,
    executables: BTreeMap<String, (PathBuf, String)>,
    version: String,
    helper_hash: String,
    launcher_hash: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LinuxUsage {
    pub cpu_usec: u64,
    pub memory_peak_bytes: u64,
    pub memory_oom_kills: u64,
    pub pids_limit_events: u64,
}
#[derive(Debug, Clone)]
pub struct LinuxInventory {
    pub stable_id: String,
    /// None means quarantined/unmatched. No resource is released for this object.
    pub execution: Option<ExecutionRef>,
    pub state: ExecutionState,
}
#[derive(Clone, Serialize, Deserialize)]
struct Record {
    plan: ResourcePlan,
    /// True until all acquisition identities and staging have been saved.
    preparing: bool,
    request: ExecutionRequest,
    observation: Observation,
    usage: Option<LinuxUsage>,
    group_inode: Option<u64>,
    scratch_mount_id: Option<u64>,
}
/// Persisted before any kernel resource is created. The delegated subtree is
/// exclusively managed by this backend; payloads cannot write either namespace.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ResourcePlan {
    control_inode: u64,
    host_epoch: Option<(String, u64)>,
    cgroup: PathBuf,
    scratch: PathBuf,
    resources: std::collections::BTreeSet<Resource>,
}
// Persist the boot and mount-namespace generation, so absence after a host reboot
// or namespace change cannot be misreported as a precreation cleanup receipt.
#[cfg(target_os = "linux")]
fn host_epoch() -> Result<Option<(String, u64)>> {
    use std::os::unix::fs::MetadataExt;
    Ok(Some((
        fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .map_err(io)?
            .trim()
            .into(),
        fs::metadata("/proc/self/ns/mnt").map_err(io)?.ino(),
    )))
}
#[cfg(not(target_os = "linux"))]
fn host_epoch() -> Result<Option<(String, u64)>> {
    Ok(None)
}
#[cfg(test)]
fn acquisition_boundary(name: &str) {
    if std::env::var("RUNTRELL_ACQUISITION_CRASH").as_deref() == Ok(name) {
        std::process::exit(73);
    }
}
fn io(e: std::io::Error) -> Error {
    Error::Invalid(format!("LINUX_IO: {e}"))
}
fn start_request(
    request: &ExecutionRequest,
    c: &ToolContext,
    began: std::time::Instant,
) -> Result<ExecutionRequest> {
    let remaining = Duration::from_millis(request.limits.wall_ms)
        .saturating_sub(began.elapsed())
        .as_millis() as u64;
    if remaining == 0 {
        return Err(Error::Deadline);
    }
    let mut effective = with_deadline(request.clone(), c)?;
    effective.limits.wall_ms = effective.limits.wall_ms.min(remaining);
    Ok(effective)
}
fn hash(path: &Path) -> Result<String> {
    Ok(crate::data::content_digest(&fs::read(path).map_err(io)?))
}
fn private(path: &Path) -> Result<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let canonical = fs::canonicalize(path).map_err(io)?;
    let m = fs::metadata(&canonical).map_err(io)?;
    if canonical != path
        || !m.is_dir()
        || m.mode() & 0o077 != 0
        || m.uid() != rustix::process::getuid().as_raw()
    {
        return Err(Error::PermissionDenied);
    }
    Ok(canonical)
}
impl LinuxBackend {
    /// root: private, durable local filesystem. cgroups: exclusively owned delegated
    /// cgroup v2 subtree, with cpu/memory/pids enabled by the host, never auto-enabled.
    /// Mounting scratch requires CAP_SYS_ADMIN in the host's mount namespace.
    pub fn new(
        root: impl AsRef<Path>,
        cgroups: impl AsRef<Path>,
        helper: impl AsRef<Path>,
        launcher: impl AsRef<Path>,
        executables: &[&str],
        bandwidth: LinuxCpuBandwidth,
    ) -> Result<Self> {
        if !cfg!(target_os = "linux") {
            return Err(Error::Unsupported("LINUX_ONLY".into()));
        }
        bandwidth.validate()?;
        let root = private(root.as_ref())?;
        let cgroups = fs::canonicalize(cgroups).map_err(io)?;
        verify_cgroup_fs(&cgroups)?;
        let enabled = fs::read_to_string(cgroups.join("cgroup.subtree_control")).map_err(io)?;
        if ["cpu", "memory", "pids"]
            .iter()
            .any(|c| !enabled.split_whitespace().any(|x| x == *c))
        {
            return Err(Error::Unsupported("CGROUP_DELEGATION".into()));
        }
        let helper = fs::canonicalize(helper).map_err(io)?;
        let launcher = fs::canonicalize(launcher).map_err(io)?;
        let helper_hash = hash(&helper)?;
        let launcher_hash = hash(&launcher)?;
        let mut allowed = BTreeMap::new();
        for executable in executables {
            let path = fs::canonicalize(executable).map_err(io)?;
            let digest = hash(&path)?;
            allowed.insert((*executable).into(), (path, digest));
        }
        let version = format!(
            "runtrell-linux-2:{}",
            digest(&json!([
                helper_hash,
                launcher_hash,
                allowed,
                cgroups,
                root,
                bandwidth
            ]))
        );
        Ok(Self {
            root,
            bandwidth,
            cgroups,
            helper,
            launcher,
            executables: allowed,
            version,
            helper_hash,
            launcher_hash,
        })
    }
    fn id(c: &ToolContext) -> String {
        key(&c.claim.run, &c.operation)
    }
    fn dir(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }
    fn group(&self, id: &str) -> PathBuf {
        self.cgroups.join(format!("rt-{id}"))
    }
    fn plan(&self, id: &str) -> Result<ResourcePlan> {
        use std::os::unix::fs::MetadataExt;
        let control = fs::symlink_metadata(self.dir(id)).map_err(io)?;
        if !control.is_dir() {
            return Err(Error::PermissionDenied);
        }
        Ok(ResourcePlan {
            control_inode: control.ino(),
            host_epoch: host_epoch()?,
            cgroup: self.group(id),
            scratch: self.dir(id).join("scratch"),
            resources: [
                Resource::Cpu,
                Resource::Pid,
                Resource::Memory,
                Resource::Disk,
                Resource::Volume,
            ]
            .into(),
        })
    }
    fn scratch_identity(&self, id: &str) -> Result<Option<u64>> {
        let path = self.dir(id).join("scratch");
        if !path.try_exists().map_err(io)? {
            return Ok(None);
        }
        if !fs::symlink_metadata(&path).map_err(io)?.is_dir() {
            return Err(Error::Conflict);
        }
        owned_mount_identity(&path, &format!("rt-{id}"))
    }
    /// Read-only ownership check also used by inventory. Missing identities can
    /// only be reconstructed for an acquisition intent in our exclusive subtree.
    fn identities(&self, id: &str, r: &Record) -> Result<(Option<u64>, Option<u64>)> {
        use std::os::unix::fs::MetadataExt;
        let reclaiming = matches!(
            r.observation.state,
            ExecutionState::Reclaiming | ExecutionState::Destroyed
        );
        let group = match fs::symlink_metadata(self.group(id)) {
            Ok(m) if m.is_dir() => Some(m.ino()),
            Ok(_) => return Err(Error::Conflict),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(io(e)),
        };
        if group.is_none() && !r.preparing && !reclaiming {
            return Err(Error::Conflict);
        }
        let mount = self.scratch_identity(id)?;
        let acquiring = r.preparing
            && matches!(
                r.observation.state,
                ExecutionState::Acquiring | ExecutionState::Unknown
            );
        for (actual, expected) in [(group, r.group_inode), (mount, r.scratch_mount_id)] {
            if actual != expected
                && !(reclaiming && actual.is_none())
                && !(acquiring && expected.is_none())
            {
                return Err(Error::Conflict);
            }
        }
        Ok((group, mount))
    }
    fn lock(&self, id: &str) -> Result<Option<File>> {
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.dir(id).join("lock"))
            .map_err(io)?;
        match f.try_lock() {
            Ok(()) => Ok(Some(f)),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(e)) => Err(io(e)),
        }
    }
    fn load(&self, id: &str) -> Result<Record> {
        if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::PermissionDenied);
        }
        let r: Record =
            serde_json::from_slice(&fs::read(self.dir(id).join("record.json")).map_err(io)?)
                .map_err(|_| Error::Storage)?;
        if key(&r.observation.execution.owner, &r.observation.execution.id) != id
            || r.observation.execution.runtime_version != self.version
            || r.plan != self.plan(id)?
        {
            return Err(Error::Conflict);
        }
        r.request.validate_input(&r.observation.execution.owner)?;
        Ok(r)
    }
    fn save(&self, id: &str, r: &Record) -> Result<()> {
        let directory = self.dir(id);
        let mut f = File::create(directory.join("record.next")).map_err(io)?;
        f.write_all(&serde_json::to_vec(r).map_err(|_| Error::Storage)?)
            .map_err(io)?;
        f.sync_all().map_err(io)?;
        fs::rename(directory.join("record.next"), directory.join("record.json")).map_err(io)?;
        File::open(directory).map_err(io)?.sync_all().map_err(io)
    }
    /// Enumerate both control records and cgroups. Malformed/missing/mismatched
    /// ownership evidence is reported as Unknown, and is never adopted or deleted.
    pub fn inventory(&self) -> Result<Vec<LinuxInventory>> {
        let mut ids = std::collections::BTreeSet::new();
        for e in fs::read_dir(&self.root).map_err(io)? {
            ids.insert(e.map_err(io)?.file_name().to_string_lossy().into_owned());
        }
        for e in fs::read_dir(&self.cgroups).map_err(io)? {
            let e = e.map_err(io)?;
            if e.file_type().map_err(io)?.is_dir() {
                let name = e.file_name().to_string_lossy().into_owned();
                ids.insert(name.strip_prefix("rt-").unwrap_or(&name).to_string());
            }
        }
        Ok(ids
            .into_iter()
            .map(|id| {
                let r = self
                    .load(&id)
                    .ok()
                    .filter(|r| self.identities(&id, r).is_ok());
                LinuxInventory {
                    stable_id: id,
                    execution: r.as_ref().map(|r| r.observation.execution.clone()),
                    state: r.map_or(ExecutionState::Unknown, |r| r.observation.state),
                }
            })
            .collect())
    }
    /// Match durable backend evidence against one pending ledger intent before
    /// management calls. The host must obtain its normal current owner fence first.
    pub fn reconcile(&self, pending: &ExecutionRecord) -> Result<Observation> {
        let id = key(&pending.owner, &pending.context_operation);
        let r = self.load(&id)?;
        if r.request != pending.request
            || pending
                .observation
                .as_ref()
                .is_some_and(|o| o.execution != r.observation.execution)
        {
            return Err(Error::Conflict);
        }
        Ok(r.observation)
    }
    pub fn usage(&self, c: &ToolContext) -> Result<LinuxUsage> {
        let id = Self::id(c);
        let r = self.load(&id)?;
        if let Some(u) = r.usage {
            return Ok(u);
        }
        self.verify_group(&id)?;
        read_usage(&self.group(&id))
    }
    fn verify_group(&self, id: &str) -> Result<()> {
        self.check_inode(id, self.load(id)?.group_inode)
    }
    fn check_inode(&self, id: &str, expected: Option<u64>) -> Result<()> {
        use std::os::unix::fs::MetadataExt;
        if Some(fs::metadata(self.group(id)).map_err(io)?.ino()) != expected {
            return Err(Error::Conflict);
        }
        Ok(())
    }
    fn prepare(&self, id: &str, row: &mut Record) -> Result<()> {
        use std::os::unix::fs::MetadataExt;
        let r = row.request.clone();
        let g = self.group(id);
        fs::create_dir(&g).map_err(io)?;
        #[cfg(test)]
        acquisition_boundary("after-cgroup");
        row.group_inode = Some(fs::metadata(&g).map_err(io)?.ino());
        self.save(id, row)?;
        // Host-selected aggregate bandwidth; cpu.stat is separately monitored for
        // cumulative CPU budget. Setup/launcher and all descendants are charged.
        for (name, value) in [
            (
                "cpu.max",
                format!(
                    "{} {}",
                    self.bandwidth.quota_usec, self.bandwidth.period_usec
                ),
            ),
            (
                "memory.max",
                r.limits.ram_bytes.ok_or(Error::Conflict)?.to_string(),
            ),
            ("memory.swap.max", "0".into()),
            ("memory.oom.group", "1".into()),
            (
                "pids.max",
                r.limits.pids.ok_or(Error::Conflict)?.to_string(),
            ),
        ] {
            fs::write(g.join(name), &value).map_err(io)?;
            if fs::read_to_string(g.join(name)).map_err(io)?.trim() != value {
                return Err(Error::Unsupported(format!("CGROUP_CONTROL: {name}")));
            }
        }
        for name in ["cgroup.kill", "cpu.stat", "memory.peak"] {
            OpenOptions::new()
                .read(name != "cgroup.kill")
                .write(name == "cgroup.kill")
                .open(g.join(name))
                .map_err(io)?;
        }
        let scratch = self.dir(id).join("scratch");
        fs::create_dir(&scratch).map_err(io)?;
        mount_scratch(
            &scratch,
            r.limits.disk_bytes.ok_or(Error::Conflict)?,
            &format!("rt-{id}"),
        )?;
        #[cfg(test)]
        acquisition_boundary("after-mount");
        row.scratch_mount_id = self.scratch_identity(id)?;
        if row.scratch_mount_id.is_none() {
            return Err(Error::Storage);
        }
        #[cfg(test)]
        acquisition_boundary("before-identity-record");
        self.save(id, row)?;
        // Populate scratch from a helper already in the group, so input tmpfs
        // pages, staging CPU and parsing allocations are included in group usage.
        if hash(&self.helper)? != self.helper_hash {
            return Err(Error::Conflict);
        }
        let status = std::process::Command::new(&self.helper)
            .arg("--stage")
            .arg(std::process::id().to_string())
            .arg(&g)
            .arg(self.dir(id))
            .arg(&scratch)
            .arg(self.dir(id).join("record.json"))
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(io)?;
        if !status.success() {
            return Err(Error::Invalid("STAGING_FAILED".into()));
        }
        Ok(())
    }
    fn command(&self, id: &str, r: &ExecutionRequest) -> Result<Command> {
        let Program::Process { executable, args } = &r.program;
        let (path, expected) = self
            .executables
            .get(executable)
            .ok_or(Error::PermissionDenied)?;
        if r.executable_hash.as_ref().is_some_and(|h| h != expected)
            || hash(path)? != *expected
            || hash(&self.helper)? != self.helper_hash
            || hash(&self.launcher)? != self.launcher_hash
        {
            return Err(Error::Conflict);
        }
        let mut cmd = Command::new(&self.helper);
        cmd.arg(u64::MAX.to_string())
            .arg(r.limits.file_bytes.unwrap_or(u64::MAX).to_string())
            .arg("--cgroup")
            .arg(std::process::id().to_string())
            .arg(self.group(id))
            .arg(self.dir(id))
            .arg(&self.launcher);
        cmd.args([
            "--unshare-all",
            // `--unshare-all` only tries a user namespace; require it so that
            // `--disable-userns` applies and the payload never runs as host root.
            "--unshare-user",
            "--uid",
            "65534",
            "--gid",
            "65534",
            "--die-with-parent",
            "--new-session",
            "--disable-userns",
            "--cap-drop",
            "ALL",
            "--proc",
            "/proc",
            "--dir",
            "/dev",
        ]);
        for device in ["null", "zero", "random", "urandom"] {
            let path = format!("/dev/{device}");
            cmd.args(["--dev-bind", &path, &path]);
        }
        // Only immutable host runtime trees are visible. No /etc, /home, credentials,
        // cgroup filesystem or control directory is mounted into the sandbox.
        for path in ["/usr", "/bin", "/lib", "/lib64"] {
            if Path::new(path).exists() {
                cmd.args(["--ro-bind", path, path]);
            }
        }
        if !["/usr/", "/bin/", "/lib/", "/lib64/"]
            .iter()
            .any(|p| path.starts_with(p))
        {
            cmd.arg("--ro-bind").arg(path).arg(path);
        }
        cmd.arg("--bind")
            .arg(self.dir(id).join("scratch"))
            .arg("/workspace")
            .args([
                "--symlink",
                "/workspace",
                "/tmp",
                "--chdir",
                "/workspace",
                "--remount-ro",
                "/",
                "--",
            ]);
        cmd.arg(path)
            .args(args)
            .env_clear()
            .env("HOME", "/workspace")
            .env("TMPDIR", "/workspace")
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        Ok(cmd)
    }
    fn seal_join(&self, id: &str) -> Result<File> {
        let cancel = File::create(self.dir(id).join("cancel")).map_err(io)?;
        cancel.sync_all().map_err(io)?;
        File::open(self.dir(id))
            .map_err(io)?
            .sync_all()
            .map_err(io)?;
        let barrier = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.dir(id).join("join.lock"))
            .map_err(io)?;
        barrier.try_lock().map_err(|_| Error::Conflict)?;
        Ok(barrier)
    }
    fn kill(&self, id: &str, r: &Record) -> Result<()> {
        let _barrier = self.seal_join(id)?;
        self.check_inode(id, r.group_inode)?;
        fs::write(self.group(id).join("cgroup.kill"), "1").map_err(io)
    }
    fn empty(&self, id: &str, r: &Record) -> Result<bool> {
        self.check_inode(id, r.group_inode)?;
        Ok(fs::read_to_string(self.group(id).join("cgroup.events"))
            .map_err(io)?
            .lines()
            .any(|l| l == "populated 0"))
    }
    fn recover_stop(&self, id: &str, r: &mut Record) -> Result<Progress> {
        if matches!(
            r.observation.state,
            ExecutionState::Stopped | ExecutionState::Destroyed | ExecutionState::Reclaiming
        ) {
            return Ok(Progress::Confirmed);
        }
        let (group, mount) = self.identities(id, r)?;
        // Save reconstructed identities before issuing any destructive syscall.
        if r.preparing {
            r.group_inode = group;
            r.scratch_mount_id = mount;
            self.save(id, r)?;
        }
        if group.is_some() {
            self.kill(id, r)?;
            if !self.empty(id, r)? {
                return Ok(Progress::Pending);
            }
            r.usage = Some(read_usage(&self.group(id))?);
        } else {
            // Only a precreation intent with no saved group identity reaches here.
            // Seal late joins even when acquisition stopped before mkdir.
            let _barrier = self.seal_join(id)?;
        }
        r.observation.state = ExecutionState::Stopped;
        r.observation.failure = Some("RECOVERED_STOP_RESULT_UNKNOWN".into());
        r.observation
            .released_resources
            .extend([Resource::Cpu, Resource::Pid]);
        if !r.preparing
            && r.scratch_mount_id.is_some()
            && self.scratch_identity(id)? == r.scratch_mount_id
        {
            let job = Entry {
                request: r.request.clone(),
                observation: r.observation.clone(),
                scratch: self.dir(id).join("scratch"),
                cancel: tokio::sync::watch::channel(false).0,
            };
            if let Ok(files) = collect_files(&job) {
                let value = json!({"stdout":[],"stderr":[],"exit":null,"files":files});
                if serde_json::to_vec(&value)
                    .is_ok_and(|b| b.len() <= r.request.limits.output_bytes)
                {
                    r.observation.result = Some(value);
                }
            }
        }
        self.save(id, r)?;
        Ok(Progress::Confirmed)
    }
    async fn run(
        self,
        id: String,
        mut r: Record,
        lock: File,
        mut cmd: Command,
        wall: u64,
    ) -> Result<()> {
        let wall_deadline = tokio::time::Instant::now() + Duration::from_millis(wall);
        let cpu = r
            .request
            .limits
            .cpu_millis
            .unwrap_or(0)
            .saturating_mul(1000);
        let mut child = match tokio::task::spawn_blocking(move || cmd.spawn()).await {
            Ok(Ok(c)) => c,
            Err(e) => {
                r.observation.failure = Some(format!("START_FAILED: {e}"));
                return self.finish(&id, &mut r, None, lock).await;
            }
            Ok(Err(e)) => {
                r.observation.failure = Some(format!("START_FAILED: {e}"));
                return self.finish(&id, &mut r, None, lock).await;
            }
        };
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let limit = r.request.limits.output_bytes / 3;
        let monitor = async {
            loop {
                let b = self.clone();
                let id = id.clone();
                let usage = tokio::task::spawn_blocking(move || {
                    if b.dir(&id).join("cancel").exists() {
                        return Err(Error::Invalid("CANCELLED".into()));
                    }
                    read_cpu_usec(&b.group(&id))
                })
                .await
                .map_err(std::io::Error::other)?
                .map_err(|e| std::io::Error::other(e.to_string()))?;
                if usage >= cpu {
                    return Err(std::io::Error::other("CPU_LIMIT"));
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        let result = tokio::select! {
            result = async { let (out,err,status)=tokio::try_join!(bounded(stdout,limit),bounded(stderr,limit),child.wait())?; Ok::<_,std::io::Error>((out,err,status)) } => result,
            result = monitor => result,
            _ = tokio::time::sleep_until(wall_deadline) => Err(std::io::Error::other("DEADLINE_EXCEEDED")),
        };
        let _ = child.start_kill();
        let stop_deadline =
            std::time::Instant::now() + Duration::from_millis(r.request.limits.stop_ms);
        // Reaping the direct child is also bounded; cgroup confirmation below
        // remains authoritative for the entire descendant tree.
        let reaped = tokio::time::timeout(
            Duration::from_millis(r.request.limits.stop_ms),
            child.wait(),
        )
        .await;
        if !matches!(reaped, Ok(Ok(_))) {
            let b = self.clone();
            return tokio::task::spawn_blocking(move || {
                let _lock = lock;
                let kill = b.kill(&id, &r);
                r.observation.state = ExecutionState::Unknown;
                r.observation.failure =
                    Some(format!("STOP_UNCONFIRMED: child wait; kill={kill:?}"));
                b.save(&id, &r)
            })
            .await
            .map_err(|_| Error::Storage)?;
        }
        let value = match result {
            Ok((out, err, status)) => {
                if !status.success() {
                    r.observation.failure = Some(format!("PROCESS_EXIT: {status}"));
                }
                Some(json!({"stdout":out,"stderr":err,"exit":status.code()}))
            }
            Err(e) => {
                r.observation.failure = Some(e.to_string());
                None
            }
        };
        self.finish_before(&id, &mut r, value, stop_deadline, lock)
            .await
    }
    async fn finish(
        &self,
        id: &str,
        r: &mut Record,
        value: Option<Value>,
        lock: File,
    ) -> Result<()> {
        let deadline = std::time::Instant::now() + Duration::from_millis(r.request.limits.stop_ms);
        self.finish_before(id, r, value, deadline, lock).await
    }
    async fn finish_before(
        &self,
        id: &str,
        r: &mut Record,
        value: Option<Value>,
        deadline: std::time::Instant,
        lock: File,
    ) -> Result<()> {
        let b = self.clone();
        let id = id.to_owned();
        let mut row = r.clone();
        let (row, result) = tokio::task::spawn_blocking(move || {
            // The writer, rather than its awaiting Future, owns the OS lock.
            // Runtime shutdown/drop must not allow another record writer in.
            let _lock = lock;
            let result = b.finish_blocking(&id, &mut row, value, deadline);
            if let Err(e) = &result {
                row.observation.state = ExecutionState::Unknown;
                row.observation.failure = Some(format!("STOP_UNCONFIRMED: {e}"));
                // Never silently leave Running when stop confirmation fails.
                // A failed durable write is still an error, never a receipt.
                b.save(&id, &row)?;
            }
            Ok::<_, Error>((row, result))
        })
        .await
        .map_err(|_| Error::Storage)??;
        *r = row;
        result
    }
    fn finish_blocking(
        &self,
        id: &str,
        r: &mut Record,
        value: Option<Value>,
        deadline: std::time::Instant,
    ) -> Result<()> {
        self.kill(id, r)?;
        // Never issue a stop receipt while any descendant remains in the cgroup.
        loop {
            if self.empty(id, r)? {
                break;
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(Error::Invalid("STOP_DEADLINE_EXCEEDED".into()));
            }
            std::thread::sleep(remaining.min(Duration::from_millis(10)));
        }
        r.usage = Some(read_usage(&self.group(id))?);
        r.observation.state = ExecutionState::Collecting;
        r.observation
            .released_resources
            .extend([Resource::Cpu, Resource::Pid]);
        self.save(id, r)?;
        let mut value = value;
        if r.observation.failure.is_none() {
            let job = Entry {
                request: r.request.clone(),
                observation: r.observation.clone(),
                scratch: self.dir(id).join("scratch"),
                cancel: tokio::sync::watch::channel(false).0,
            };
            match collect_files(&job) {
                Ok(files) => {
                    if let Some(v) = &mut value {
                        v["files"] = json!(files);
                    }
                }
                Err(e) => r.observation.failure = Some(e.to_string()),
            }
        }
        if value.as_ref().is_some_and(|v| {
            serde_json::to_vec(v).map_or(true, |b| b.len() > r.request.limits.output_bytes)
        }) {
            value = None;
            r.observation.failure = Some("OUTPUT_LIMIT".into());
        }
        r.observation.result = value;
        r.observation.state = ExecutionState::Stopped;
        self.save(id, r)
    }
}
impl ExecutionBackend for LinuxBackend {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            isolation: Isolation::Untrusted,
            cpu: true,
            ram: true,
            pids: true,
            disk: true,
            file: true,
            network_deny: true,
            runtime_version: self.version.clone(),
        }
    }
    fn supports(&self, r: &ExecutionRequest) -> Result<()> {
        self.capabilities().check(r)?;
        if [
            r.limits.cpu_millis,
            r.limits.ram_bytes,
            r.limits.pids,
            r.limits.disk_bytes,
        ]
        .iter()
        .any(Option::is_none)
        {
            return Err(Error::Invalid("LINUX_REQUIRES_ALL_LIMITS".into()));
        }
        if r.limits
            .disk_bytes
            .is_some_and(|n| !n.is_multiple_of(rustix::param::page_size() as u64))
        {
            return Err(Error::Invalid("LINUX_DISK_PAGE_ALIGNMENT".into()));
        }
        let Program::Process { executable, args } = &r.program;
        if !self.executables.contains_key(executable) {
            return Err(Error::PermissionDenied);
        }
        if args.len() > 256
            || args.iter().map(String::len).sum::<usize>() > 65536
            || r.output_paths.len() > 256
        {
            return Err(Error::Invalid("PROCESS_ARGUMENTS".into()));
        }
        for p in &r.output_paths {
            crate::data::logical_path(p)?;
        }
        Ok(())
    }
    async fn acquire(&self, c: &ToolContext, r: &ExecutionRequest) -> Result<ExecutionRef> {
        self.supports(r)?;
        r.validate_owner(c)?;
        let b = self.clone();
        let c = c.clone();
        let r = r.clone();
        tokio::task::spawn_blocking(move || {
            let id = Self::id(&c);
            let dir = b.dir(&id);
            match fs::create_dir(&dir) {
                Ok(()) => {
                    File::open(&b.root).map_err(io)?.sync_all().map_err(io)?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
                Err(e) => return Err(io(e)),
            }
            let _lock = b.lock(&id)?.ok_or(Error::Conflict)?;
            if dir.join("record.json").exists() {
                let old = b.load(&id)?;
                if old.request != r {
                    return Err(Error::Conflict);
                }
                return Ok(old.observation.execution);
            }
            // Existing unowned scratch/cgroup is quarantined, never adopted.
            if b.group(&id).exists() || dir.join("scratch").exists() {
                return Err(Error::Conflict);
            }
            let execution = ExecutionRef {
                owner: c.claim.run,
                id: c.operation,
                runtime_version: b.version.clone(),
            };
            let mut row = Record {
                plan: b.plan(&id)?,
                preparing: true,
                request: r.clone(),
                observation: Observation {
                    execution: execution.clone(),
                    state: ExecutionState::Acquiring,
                    result: None,
                    failure: None,
                    cost_micro_usd: Some(0),
                    released_resources: Default::default(),
                },
                usage: None,
                group_inode: None,
                scratch_mount_id: None,
            };
            b.save(&id, &row)?;
            if let Err(e) = b.prepare(&id, &mut row) {
                row.observation.state = ExecutionState::Unknown;
                row.observation.failure = Some(format!("ACQUIRE_FAILED: {e}"));
                b.save(&id, &row)?;
                return Err(e);
            }
            row.preparing = false;
            row.observation.state = ExecutionState::Ready;
            b.save(&id, &row)?;
            Ok(execution)
        })
        .await
        .map_err(|_| Error::Storage)?
    }
    async fn start(&self, c: &ToolContext, e: &ExecutionRef) -> Result<()> {
        let began = std::time::Instant::now();
        loop {
            let backend = self.clone();
            let c = c.clone();
            let e = e.clone();
            let started = tokio::task::spawn_blocking(move || {
                let id = Self::id(&c);
                let mut r = backend.load(&id)?;
                if r.observation.execution != e {
                    return Err(Error::PermissionDenied);
                }
                let lock = backend.lock(&id)?;
                if lock.is_some() {
                    r = backend.load(&id)?;
                }
                match r.observation.state {
                    ExecutionState::Ready => (),
                    ExecutionState::Acquiring | ExecutionState::Unknown => {
                        return Err(Error::Conflict);
                    }
                    ExecutionState::Running
                    | ExecutionState::Collecting
                    | ExecutionState::Stopped
                    | ExecutionState::Reclaiming
                    | ExecutionState::Destroyed => return Ok(true),
                }
                if backend.dir(&id).join("cancel").exists() {
                    return Err(Error::Closed);
                }
                let effective = start_request(&r.request, &c, began)?;
                let Some(lock) = lock else {
                    // flock contention is not a supervisor acknowledgement. In
                    // particular, a different Run's fork can temporarily retain
                    // an acquisition descriptor even after Ready was saved.
                    return Ok(false);
                };
                backend.verify_group(&id)?;
                if r.scratch_mount_id.is_none()
                    || backend.scratch_identity(&id)? != r.scratch_mount_id
                {
                    return Err(Error::Conflict);
                }
                if read_usage(&backend.group(&id))?.cpu_usec
                    >= effective
                        .limits
                        .cpu_millis
                        .ok_or(Error::Conflict)?
                        .saturating_mul(1000)
                {
                    backend.recover_stop(&id, &mut r)?;
                    return Err(Error::BudgetAtLimit);
                }
                let command = backend.command(&id, &effective)?;
                let effective = start_request(&r.request, &c, began)?;
                r.observation.state = ExecutionState::Running;
                backend.save(&id, &r)?;
                // The persisted request remains fixed. Only this watchdog uses remaining time.
                let b = backend.clone();
                tokio::spawn(
                    async move { b.run(id, r, lock, command, effective.limits.wall_ms).await },
                );
                Ok(true)
            })
            .await
            .map_err(|_| Error::Storage)??;
            if started {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
    async fn lookup(&self, c: &ToolContext) -> Result<Option<Observation>> {
        let backend = self.clone();
        let c = c.clone();
        tokio::task::spawn_blocking(move || {
            let id = Self::id(&c);
            if !backend.dir(&id).exists() {
                return Ok(None);
            }
            // Malformed/mismatched evidence is an error, never a missing-resource receipt.
            Ok(Some(backend.load(&id)?.observation))
        })
        .await
        .map_err(|_| Error::Storage)?
    }
    async fn cancel(&self, c: &ToolContext) -> Result<()> {
        let backend = self.clone();
        let c = c.clone();
        tokio::task::spawn_blocking(move || {
            let id = Self::id(&c);
            backend.load(&id)?;
            let f = File::create(backend.dir(&id).join("cancel")).map_err(io)?;
            f.sync_all().map_err(io)?;
            File::open(backend.dir(&id))
                .map_err(io)?
                .sync_all()
                .map_err(io)
        })
        .await
        .map_err(|_| Error::Storage)?
    }
    async fn stop(&self, c: &ToolContext) -> Result<Progress> {
        let id = Self::id(c);
        let dir = self.dir(&id);
        if !tokio::task::spawn_blocking(move || dir.exists())
            .await
            .map_err(|_| Error::Storage)?
        {
            return Ok(Progress::Unknown);
        }
        self.cancel(c).await?;
        let backend = self.clone();
        tokio::task::spawn_blocking(move || {
            let Some(_lock) = backend.lock(&id)? else {
                // Acquisition can still own the operation lock while a staging
                // helper is running. Kill the verified group without publishing
                // a receipt or racing its record writer. The join barrier also
                // prevents any not-yet-attached helper from starting later.
                let r = backend.load(&id)?;
                if r.group_inode.is_some() {
                    backend.kill(&id, &r)?;
                }
                return Ok(Progress::Pending);
            };
            let mut r = backend.load(&id)?;
            backend.recover_stop(&id, &mut r)
        })
        .await
        .map_err(|_| Error::Storage)?
    }
    async fn destroy(&self, c: &ToolContext) -> Result<Progress> {
        let backend = self.clone();
        let c = c.clone();
        tokio::task::spawn_blocking(move || {
            let id = Self::id(&c);
            if !backend.dir(&id).exists() {
                return Ok(Progress::Unknown);
            }
            let Some(_lock) = backend.lock(&id)? else {
                return Ok(Progress::Pending);
            };
            let mut r = backend.load(&id)?;
            if r.observation.state == ExecutionState::Destroyed {
                return Ok(Progress::Confirmed);
            }
            if !matches!(
                r.observation.state,
                ExecutionState::Stopped | ExecutionState::Reclaiming
            ) {
                return Ok(Progress::Unknown);
            }
            let (group, mounted) = backend.identities(&id, &r)?;
            if group.is_some() && !backend.empty(&id, &r)? {
                return Ok(Progress::Unknown);
            }
            if r.observation.state == ExecutionState::Stopped {
                r.observation.state = ExecutionState::Reclaiming;
                backend.save(&id, &r)?;
            }
            let scratch = backend.dir(&id).join("scratch");
            if mounted.is_some() {
                unmount_scratch(&scratch)?;
            }
            if backend.scratch_identity(&id)?.is_some() {
                return Err(Error::Conflict);
            }
            match fs::remove_dir(&scratch) {
                Ok(()) => (),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => return Err(io(e)),
            }
            match fs::remove_dir(backend.group(&id)) {
                Ok(()) => (),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => return Err(io(e)),
            }
            r.observation.state = ExecutionState::Destroyed;
            r.observation.local_destroy();
            r.observation.released_resources.insert(Resource::Memory);
            backend.save(&id, &r)?;
            Ok(Progress::Confirmed)
        })
        .await
        .map_err(|_| Error::Storage)?
    }
}
fn read_usage(group: &Path) -> Result<LinuxUsage> {
    let cpu_usec = read_cpu_usec(group)?;
    let memory_peak_bytes = fs::read_to_string(group.join("memory.peak"))
        .map_err(io)?
        .trim()
        .parse()
        .map_err(|_| Error::Storage)?;
    fn event(group: &Path, file: &str, name: &str) -> Result<u64> {
        fs::read_to_string(group.join(file))
            .map_err(io)?
            .lines()
            .find_map(|l| l.strip_prefix(name))
            .ok_or(Error::Storage)?
            .parse()
            .map_err(|_| Error::Storage)
    }
    Ok(LinuxUsage {
        cpu_usec,
        memory_peak_bytes,
        memory_oom_kills: event(group, "memory.events", "oom_kill ")?,
        pids_limit_events: event(group, "pids.events", "max ")?,
    })
}
fn read_cpu_usec(group: &Path) -> Result<u64> {
    let stat = fs::read_to_string(group.join("cpu.stat")).map_err(io)?;
    stat.lines()
        .find_map(|l| l.strip_prefix("usage_usec "))
        .ok_or(Error::Storage)?
        .parse()
        .map_err(|_| Error::Storage)
}
#[cfg(target_os = "linux")]
fn verify_cgroup_fs(path: &Path) -> Result<()> {
    if rustix::fs::statfs(path).map_err(|_| Error::Storage)?.f_type != 0x63677270 {
        return Err(Error::Unsupported("CGROUP_V2_REQUIRED".into()));
    }
    Ok(())
}
#[cfg(not(target_os = "linux"))]
fn verify_cgroup_fs(_: &Path) -> Result<()> {
    Err(Error::Unsupported("LINUX_ONLY".into()))
}
#[cfg(target_os = "linux")]
fn mount_scratch(path: &Path, bytes: u64, source: &str) -> Result<()> {
    use rustix::mount::{MountFlags, mount};
    let options = std::ffi::CString::new(format!("size={bytes},nr_inodes=4096,mode=0700"))
        .map_err(|_| Error::Storage)?;
    mount(
        source,
        path,
        "tmpfs",
        MountFlags::NODEV | MountFlags::NOSUID,
        options.as_c_str(),
    )
    .map_err(|e| io(e.into()))
}
#[cfg(not(target_os = "linux"))]
fn mount_scratch(_: &Path, _: u64, _: &str) -> Result<()> {
    Err(Error::Unsupported("LINUX_ONLY".into()))
}
#[cfg(target_os = "linux")]
fn unmount_scratch(path: &Path) -> Result<()> {
    use rustix::mount::{UnmountFlags, unmount};
    // Never lazy-unmount: busy scratch retains its disk reservation.
    match unmount(path, UnmountFlags::empty()) {
        Ok(()) | Err(rustix::io::Errno::INVAL) | Err(rustix::io::Errno::NOENT) => Ok(()),
        Err(e) => Err(io(e.into())),
    }
}
#[cfg(not(target_os = "linux"))]
fn unmount_scratch(_: &Path) -> Result<()> {
    Err(Error::Unsupported("LINUX_ONLY".into()))
}

#[cfg(any(target_os = "linux", test))]
fn parse_owned_mount(info: &str, path: &Path, source: &str) -> Result<Option<u64>> {
    let mut found = None;
    for line in info.lines() {
        let (before, after) = line.split_once(" - ").ok_or(Error::Storage)?;
        let fields = before.split_whitespace().collect::<Vec<_>>();
        let filesystem = after.split_whitespace().collect::<Vec<_>>();
        if fields.len() < 6 || filesystem.len() < 3 {
            return Err(Error::Storage);
        }
        let target = fields[4]
            .replace("\\040", " ")
            .replace("\\011", "\t")
            .replace("\\012", "\n")
            .replace("\\134", "\\");
        if Path::new(&target) == path {
            if filesystem[0] != "tmpfs" || filesystem[1] != source || found.is_some() {
                return Err(Error::Conflict);
            }
            found = Some(fields[0].parse().map_err(|_| Error::Storage)?);
        }
    }
    Ok(found)
}
#[cfg(target_os = "linux")]
fn owned_mount_identity(path: &Path, source: &str) -> Result<Option<u64>> {
    parse_owned_mount(
        &fs::read_to_string("/proc/self/mountinfo").map_err(io)?,
        path,
        source,
    )
}
#[cfg(not(target_os = "linux"))]
fn owned_mount_identity(_: &Path, _: &str) -> Result<Option<u64>> {
    Err(Error::Unsupported("LINUX_ONLY".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(root: &Path, groups: &Path) -> LinuxBackend {
        LinuxBackend {
            root: root.into(),
            bandwidth: LinuxCpuBandwidth {
                quota_usec: 10000,
                period_usec: 10000,
            },
            cgroups: groups.into(),
            helper: PathBuf::new(),
            launcher: PathBuf::new(),
            executables: BTreeMap::new(),
            version: "test-version".into(),
            helper_hash: String::new(),
            launcher_hash: String::new(),
        }
    }
    fn ready_record(b: &LinuxBackend, c: &ToolContext) -> Record {
        let id = LinuxBackend::id(c);
        fs::create_dir(b.dir(&id)).unwrap();
        let row = Record {
            plan: b.plan(&id).unwrap(),
            preparing: false,
            request: ExecutionRequest::process("/bin/sh", vec![]),
            observation: Observation {
                execution: ExecutionRef {
                    owner: c.claim.run.clone(),
                    id: c.operation.clone(),
                    runtime_version: b.version.clone(),
                },
                state: ExecutionState::Ready,
                result: None,
                failure: None,
                cost_micro_usd: Some(0),
                released_resources: Default::default(),
            },
            usage: None,
            group_inode: None,
            scratch_mount_id: None,
        };
        b.save(&id, &row).unwrap();
        row
    }
    #[tokio::test]
    async fn start_never_acknowledges_ready_while_operation_lock_is_held() {
        let root = tempfile::tempdir().unwrap();
        let groups = tempfile::tempdir().unwrap();
        let b = fixture(root.path(), groups.path());
        let c = crash_context();
        let id = LinuxBackend::id(&c);
        let mut row = ready_record(&b, &c);
        row.request.limits.wall_ms = 30;
        b.save(&id, &row).unwrap();
        let acquisition_lock = b.lock(&id).unwrap().unwrap();
        // Like a different Run's forked child before exec: closing the parent's
        // descriptor does not release flock while another descriptor owns it.
        let inherited_lock = acquisition_lock.try_clone().unwrap();
        drop(acquisition_lock);
        assert!(b.lock(&id).unwrap().is_none());
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            b.start(&c, &row.observation.execution),
        )
        .await
        .expect("contention must not reset the request's wall budget");
        assert!(matches!(result, Err(Error::Deadline)), "{result:?}");
        let observation = b.lookup(&c).await.unwrap().unwrap();
        assert_eq!(observation.state, ExecutionState::Ready);
        assert!(observation.released_resources.is_empty());
        drop(inherited_lock);
    }
    #[tokio::test]
    async fn start_retries_ready_after_lock_release_and_checks_group_identity() {
        let root = tempfile::tempdir().unwrap();
        let groups = tempfile::tempdir().unwrap();
        let b = fixture(root.path(), groups.path());
        let c = crash_context();
        let id = LinuxBackend::id(&c);
        let row = ready_record(&b, &c);
        fs::create_dir(b.group(&id)).unwrap();
        fs::write(b.group(&id).join("cgroup.kill"), "untouched").unwrap();
        let lock = b.lock(&id).unwrap().unwrap();
        let start = b.start(&c, &row.observation.execution);
        tokio::pin!(start);
        tokio::select! {
            result = &mut start => panic!("Ready is still locked: {result:?}"),
            _ = tokio::time::sleep(Duration::from_millis(20)) => (),
        }
        drop(lock);
        let result = tokio::time::timeout(Duration::from_secs(1), &mut start)
            .await
            .expect("start must retry after the lock is released");
        // The group has no saved inode: retry must reach the real preflight,
        // rather than acknowledge Ready or adopt a group by name.
        assert!(matches!(result, Err(Error::Conflict)), "{result:?}");
        assert_eq!(
            fs::read(b.group(&id).join("cgroup.kill")).unwrap(),
            b"untouched"
        );
        let saved = b.load(&id).unwrap();
        assert_eq!(saved.request, row.request);
        assert_eq!(saved.observation, row.observation);
    }
    #[tokio::test]
    async fn start_acknowledges_competing_supervisor_without_waiting_for_its_lock() {
        let root = tempfile::tempdir().unwrap();
        let groups = tempfile::tempdir().unwrap();
        let b = fixture(root.path(), groups.path());
        let c = crash_context();
        let id = LinuxBackend::id(&c);
        let mut row = ready_record(&b, &c);
        let lock = b.lock(&id).unwrap().unwrap();
        let execution = row.observation.execution.clone();
        let start = b.start(&c, &execution);
        tokio::pin!(start);
        tokio::select! {
            result = &mut start => panic!("supervisor has not saved Running: {result:?}"),
            _ = tokio::time::sleep(Duration::from_millis(20)) => (),
        }
        // Fake supervisor: publish its durable handoff, retaining its lock.
        // No Linux-only mount or process launch is needed for this race.
        row.observation.state = ExecutionState::Running;
        b.save(&id, &row).unwrap();
        tokio::time::timeout(Duration::from_secs(1), &mut start)
            .await
            .unwrap()
            .unwrap();
        assert!(b.lock(&id).unwrap().is_none());
        assert_eq!(b.load(&id).unwrap().observation, row.observation);
        // An idempotent replay must not create another supervisor either.
        b.start(&c, &execution).await.unwrap();
        drop(lock);
    }
    #[tokio::test]
    async fn start_contention_honors_context_deadline_and_cancel() {
        for cancel in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let groups = tempfile::tempdir().unwrap();
            let b = fixture(root.path(), groups.path());
            let mut c = crash_context();
            let id = LinuxBackend::id(&c);
            let row = ready_record(&b, &c);
            let _lock = b.lock(&id).unwrap().unwrap();
            c.deadline = Some(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64
                    + 30,
            );
            if cancel {
                b.cancel(&c).await.unwrap();
            }
            let result = tokio::time::timeout(
                Duration::from_millis(500),
                b.start(&c, &row.observation.execution),
            )
            .await
            .expect("the context deadline must bound the Ready lock wait");
            if cancel {
                assert!(matches!(result, Err(Error::Closed)), "{result:?}");
            } else {
                assert!(matches!(result, Err(Error::Deadline)), "{result:?}");
            }
            assert_eq!(b.load(&id).unwrap().observation, row.observation);
        }
    }
    #[tokio::test]
    async fn start_rejects_unconfirmed_states_and_foreign_reference() {
        let root = tempfile::tempdir().unwrap();
        let groups = tempfile::tempdir().unwrap();
        let b = fixture(root.path(), groups.path());
        let c = crash_context();
        let id = LinuxBackend::id(&c);
        let mut row = ready_record(&b, &c);
        for state in [ExecutionState::Acquiring, ExecutionState::Unknown] {
            row.observation.state = state;
            b.save(&id, &row).unwrap();
            for contended in [false, true] {
                let _lock = contended.then(|| b.lock(&id).unwrap().unwrap());
                assert!(matches!(
                    b.start(&c, &row.observation.execution).await,
                    Err(Error::Conflict)
                ));
                assert_eq!(b.load(&id).unwrap().observation, row.observation);
            }
        }
        row.observation.state = ExecutionState::Running;
        b.save(&id, &row).unwrap();
        let _lock = b.lock(&id).unwrap().unwrap();
        let mut foreign = row.observation.execution.clone();
        foreign.owner.id = "another-run".into();
        assert!(matches!(
            b.start(&c, &foreign).await,
            Err(Error::PermissionDenied)
        ));
    }
    #[test]
    fn durable_identity_lock_and_ledger_match_survive_client_loss() {
        let root = tempfile::tempdir().unwrap();
        let groups = tempfile::tempdir().unwrap();
        let b = fixture(root.path(), groups.path());
        let owner = RunHandle {
            tenant: "t".into(),
            id: "r".into(),
        };
        let id = key(&owner, "op");
        fs::create_dir(b.dir(&id)).unwrap();
        let lock = b.lock(&id).unwrap().unwrap();
        assert!(b.lock(&id).unwrap().is_none());
        let request = ExecutionRequest::process("/bin/sh", vec![]);
        let row = Record {
            plan: b.plan(&id).unwrap(),
            preparing: false,
            request: request.clone(),
            observation: Observation {
                execution: ExecutionRef {
                    owner: owner.clone(),
                    id: "op".into(),
                    runtime_version: b.version.clone(),
                },
                state: ExecutionState::Running,
                result: None,
                failure: None,
                cost_micro_usd: Some(0),
                released_resources: Default::default(),
            },
            usage: None,
            group_inode: None,
            scratch_mount_id: None,
        };
        b.save(&id, &row).unwrap();
        drop(lock);
        drop(b);
        let b = fixture(root.path(), groups.path());
        assert!(b.lock(&id).unwrap().is_some());
        let mut pending = ExecutionRecord {
            context_operation: "op".into(),
            owner,
            request,
            revision: 0,
            cancel_requested: false,
            observation: None,
            resources: BTreeMap::new(),
        };
        assert_eq!(
            b.reconcile(&pending).unwrap().state,
            ExecutionState::Running
        );
        pending.request.limits.wall_ms += 1;
        assert!(b.reconcile(&pending).is_err());
        // A replaced group must not receive a kill, even with the same stable name.
        fs::create_dir(b.group(&id)).unwrap();
        fs::write(b.group(&id).join("cgroup.kill"), b"untouched").unwrap();
        assert!(b.kill(&id, &row).is_err());
        assert_eq!(
            fs::read(b.group(&id).join("cgroup.kill")).unwrap(),
            b"untouched"
        );
        // Unmatched cgroup and corrupt record stay quarantined with no receipt.
        fs::create_dir(groups.path().join("rt-orphan")).unwrap();
        let inventory = b.inventory().unwrap();
        assert!(inventory.iter().any(|i| i.stable_id == "orphan"
            && i.state == ExecutionState::Unknown
            && i.execution.is_none()));
        fs::write(b.dir(&id).join("record.json"), b"corrupt").unwrap();
        assert!(b.inventory().unwrap().iter().any(|i| i.stable_id == id
            && i.execution.is_none()
            && i.state == ExecutionState::Unknown));
        assert!(b.reconcile(&pending).is_err());
        assert!(groups.path().join("rt-orphan").exists());
    }
    #[test]
    fn precreation_intent_is_discoverable_without_resource_identities() {
        let root = tempfile::tempdir().unwrap();
        let groups = tempfile::tempdir().unwrap();
        let b = fixture(root.path(), groups.path());
        let owner = RunHandle {
            tenant: "t".into(),
            id: "r".into(),
        };
        let id = key(&owner, "precreation");
        fs::create_dir(b.dir(&id)).unwrap();
        let row = Record {
            plan: b.plan(&id).unwrap(),
            preparing: true,
            request: ExecutionRequest::process("/bin/sh", vec![]),
            observation: Observation {
                execution: ExecutionRef {
                    owner,
                    id: "precreation".into(),
                    runtime_version: b.version.clone(),
                },
                state: ExecutionState::Acquiring,
                result: None,
                failure: None,
                cost_micro_usd: Some(0),
                released_resources: Default::default(),
            },
            usage: None,
            group_inode: None,
            scratch_mount_id: None,
        };
        b.save(&id, &row).unwrap();
        let inventory = b.inventory().unwrap();
        assert_eq!(inventory.len(), 1);
        assert_eq!(inventory[0].execution, Some(row.observation.execution));
        assert_eq!(inventory[0].state, ExecutionState::Acquiring);
        let mut tampered = b.load(&id).unwrap();
        tampered.plan.cgroup = groups.path().join("foreign");
        b.save(&id, &tampered).unwrap();
        assert!(b.load(&id).is_err());
        assert!(b.inventory().unwrap()[0].execution.is_none());
        tampered.plan = b.plan(&id).unwrap();
        tampered.plan.host_epoch = Some(("another-boot".into(), 0));
        b.save(&id, &tampered).unwrap();
        assert!(b.load(&id).is_err());
        tampered.plan = b.plan(&id).unwrap();
        b.save(&id, &tampered).unwrap();
        fs::create_dir(b.group(&id)).unwrap();
        assert!(b.identities(&id, &tampered).unwrap().0.is_some());
        tampered.observation.state = ExecutionState::Stopped;
        assert!(
            b.identities(&id, &tampered).is_err(),
            "never adopt a new identity after stop"
        );
        tampered.observation.state = ExecutionState::Acquiring;
        tampered.group_inode = Some(0);
        assert!(
            b.identities(&id, &tampered).is_err(),
            "never replace a saved inode"
        );
    }
    #[tokio::test]
    async fn precreation_recovery_confirms_absence_and_reclaims_idempotently() {
        let root = tempfile::tempdir().unwrap();
        let groups = tempfile::tempdir().unwrap();
        let b = fixture(root.path(), groups.path());
        let c = crash_context();
        let id = LinuxBackend::id(&c);
        fs::create_dir(b.dir(&id)).unwrap();
        let row = Record {
            plan: b.plan(&id).unwrap(),
            preparing: true,
            request: ExecutionRequest::process("/bin/sh", vec![]),
            observation: Observation {
                execution: ExecutionRef {
                    owner: c.claim.run.clone(),
                    id: c.operation.clone(),
                    runtime_version: b.version.clone(),
                },
                state: ExecutionState::Acquiring,
                result: None,
                failure: None,
                cost_micro_usd: Some(0),
                released_resources: Default::default(),
            },
            usage: None,
            group_inode: None,
            scratch_mount_id: None,
        };
        b.save(&id, &row).unwrap();
        assert_eq!(b.stop(&c).await.unwrap(), Progress::Confirmed);
        let stopped = b.lookup(&c).await.unwrap().unwrap();
        assert_eq!(
            stopped.released_resources,
            [Resource::Cpu, Resource::Pid].into()
        );
        assert!(
            b.usage(&c).is_err(),
            "no invented usage for a group never created"
        );
        assert_eq!(b.destroy(&c).await.unwrap(), Progress::Confirmed);
        assert_eq!(b.destroy(&c).await.unwrap(), Progress::Confirmed);
        assert_eq!(
            b.lookup(&c).await.unwrap().unwrap().released_resources,
            row.plan.resources
        );
        // A group without intent is reported, never stopped or deleted.
        let orphan = key(&c.claim.run, "orphan");
        fs::create_dir(b.group(&orphan)).unwrap();
        assert!(
            b.inventory()
                .unwrap()
                .iter()
                .any(|i| i.stable_id == orphan && i.execution.is_none())
        );
        let mut other = c.clone();
        other.operation = "orphan".into();
        assert_eq!(b.stop(&other).await.unwrap(), Progress::Unknown);
        assert_eq!(b.destroy(&other).await.unwrap(), Progress::Unknown);
        assert!(b.group(&orphan).exists());
    }
    #[test]
    fn mount_provenance_rejects_foreign_source_type_and_stacked_mounts() {
        let path = Path::new("/state/with space/scratch");
        let line = "42 1 0:3 / /state/with\\040space/scratch rw - tmpfs rt-operation rw\n";
        assert_eq!(
            parse_owned_mount(line, path, "rt-operation").unwrap(),
            Some(42)
        );
        assert!(parse_owned_mount(line, path, "rt-other").is_err());
        assert!(parse_owned_mount(&line.replace("tmpfs", "ext4"), path, "rt-operation").is_err());
        assert!(parse_owned_mount(&format!("{line}{line}"), path, "rt-operation").is_err());
        assert_eq!(
            parse_owned_mount(line, Path::new("/other"), "rt-operation").unwrap(),
            None
        );
    }
    fn crash_context() -> ToolContext {
        ToolContext {
            recovery_proof: None,
            claim: Claim {
                run: RunHandle {
                    tenant: "crash-test".into(),
                    id: "run".into(),
                },
                worker: "worker".into(),
                generation: 1,
                lease_until: u64::MAX,
            },
            principal: "host".into(),
            operation: "acquisition-crash".into(),
            attempt: 1,
            deadline: None,
            permission: "execute".into(),
            account_binding: "test".into(),
        }
    }
    fn privileged_backend(root: &Path) -> LinuxBackend {
        assert_eq!(std::env::var("RUNTRELL_LINUX_TEST").as_deref(), Ok("1"));
        LinuxBackend::new(
            root,
            std::env::var("RUNTRELL_CGROUP_ROOT").unwrap(),
            "/usr/bin/true",
            "/usr/bin/bwrap",
            &["/bin/sh"],
            LinuxCpuBandwidth {
                quota_usec: 10000,
                period_usec: 10000,
            },
        )
        .unwrap()
    }
    async fn acquisition_crash(boundary: &str, name: &str) {
        let c = crash_context();
        let mut request = ExecutionRequest::process("/bin/sh", vec![]);
        request.limits.cpu_millis = Some(1000);
        request.limits.ram_bytes = Some(128 * 1024 * 1024);
        request.limits.pids = Some(32);
        request.limits.disk_bytes = Some(4 * 1024 * 1024);
        if let Ok(root) = std::env::var("RUNTRELL_ACQUISITION_CHILD_ROOT") {
            privileged_backend(Path::new(&root))
                .acquire(&c, &request)
                .await
                .unwrap();
            panic!("crash boundary was not reached");
        }
        let root = tempfile::Builder::new()
            .prefix("acquisition-")
            .tempdir_in(std::env::var("RUNTRELL_LINUX_STATE_ROOT").unwrap())
            .unwrap()
            .keep();
        {
            use std::os::unix::fs::PermissionsExt;
            // The backend accepts only a private (0700) state root.
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        }
        // The parent ledger survives the backend child process. Pg ledger durability
        // is covered by the independent shared Memory/Pg contract suite.
        let ledger = MemoryExecutionLedger::default();
        let pending = ExecutionRecord::intent(&c, request.clone());
        ledger.insert(pending.clone()).await.unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                &format!("execution::linux::tests::{name}"),
            ])
            .env("RUNTRELL_ACQUISITION_CHILD_ROOT", &root)
            .env("RUNTRELL_ACQUISITION_CRASH", boundary)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(73), "{output:?}");
        let b = privileged_backend(&root);
        let id = LinuxBackend::id(&c);
        let before = b.load(&id).unwrap();
        assert!(before.preparing);
        assert_eq!(before.observation.state, ExecutionState::Acquiring);
        assert!(before.observation.released_resources.is_empty());
        assert!(b.group(&id).exists(), "must really create the cgroup");
        if boundary == "after-cgroup" {
            assert_eq!(before.group_inode, None);
            assert!(!b.dir(&id).join("scratch").exists());
        } else {
            assert!(before.group_inode.is_some());
            assert_eq!(before.scratch_mount_id, None);
            assert!(
                b.scratch_identity(&id).unwrap().is_some(),
                "must really mount scratch"
            );
        }
        assert!(
            b.inventory()
                .unwrap()
                .iter()
                .any(|i| i.stable_id == id && i.execution.is_some())
        );
        assert_eq!(
            b.reconcile(&ledger.pending().await.unwrap()[0])
                .unwrap()
                .execution
                .id,
            c.operation
        );
        let mut wrong = pending;
        wrong.owner.id = "other-run".into();
        assert!(b.reconcile(&wrong).is_err());
        assert_eq!(b.stop(&c).await.unwrap(), Progress::Confirmed);
        let stopped = b.lookup(&c).await.unwrap().unwrap();
        assert_eq!(
            stopped.released_resources,
            [Resource::Cpu, Resource::Pid].into()
        );
        drop(b);
        let b = privileged_backend(&root);
        assert_eq!(b.destroy(&c).await.unwrap(), Progress::Confirmed);
        assert_eq!(b.destroy(&c).await.unwrap(), Progress::Confirmed);
        assert_eq!(b.stop(&c).await.unwrap(), Progress::Confirmed);
        assert!(!b.group(&id).exists());
        assert!(!b.dir(&id).join("scratch").exists());
        assert_eq!(b.scratch_identity(&id).unwrap(), None);
        assert_eq!(
            b.lookup(&c).await.unwrap().unwrap().released_resources,
            before.plan.resources
        );
        assert_eq!(
            b.acquire(&c, &request).await.unwrap(),
            before.observation.execution
        );
        // No intent: leave an actual unknown cgroup quarantined; never attach it
        // to another Run. Remove only the empty fixture group after assertions.
        let orphan = key(&c.claim.run, "unowned");
        fs::create_dir(b.group(&orphan)).unwrap();
        fs::create_dir(b.dir(&orphan)).unwrap();
        let foreign_scratch = b.dir(&orphan).join("scratch");
        fs::create_dir(&foreign_scratch).unwrap();
        mount_scratch(&foreign_scratch, 4 * 1024 * 1024, "unowned-fixture").unwrap();
        assert!(b.inventory().unwrap().iter().any(|i| i.stable_id == orphan
            && i.execution.is_none()
            && i.state == ExecutionState::Unknown));
        let mut other = c.clone();
        other.operation = "unowned".into();
        assert!(b.acquire(&other, &request).await.is_err());
        assert!(b.stop(&other).await.is_err());
        assert!(b.destroy(&other).await.is_err());
        assert!(b.group(&orphan).exists());
        assert!(
            owned_mount_identity(&foreign_scratch, "unowned-fixture")
                .unwrap()
                .is_some()
        );
        unmount_scratch(&foreign_scratch).unwrap();
        fs::remove_dir(b.group(&orphan)).unwrap();
        fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    #[ignore = "requires privileged Linux cgroup v2 and mount namespace; crashes child process"]
    async fn linux_acquisition_crash_after_cgroup() {
        acquisition_crash("after-cgroup", "linux_acquisition_crash_after_cgroup").await;
    }
    #[tokio::test]
    #[ignore = "requires privileged Linux cgroup v2 and mount namespace; crashes child process"]
    async fn linux_acquisition_crash_after_mount() {
        acquisition_crash("after-mount", "linux_acquisition_crash_after_mount").await;
    }
    #[tokio::test]
    #[ignore = "requires privileged Linux cgroup v2 and mount namespace; crashes child process"]
    async fn linux_acquisition_crash_before_identity_record() {
        acquisition_crash(
            "before-identity-record",
            "linux_acquisition_crash_before_identity_record",
        )
        .await;
    }
    #[tokio::test]
    async fn stop_timeout_is_durable_and_later_recovery_confirms_only_cpu_pid() {
        use std::os::unix::fs::MetadataExt;
        let root = tempfile::tempdir().unwrap();
        let groups = tempfile::tempdir().unwrap();
        let b = fixture(root.path(), groups.path());
        let owner = RunHandle {
            tenant: "t".into(),
            id: "r".into(),
        };
        let id = key(&owner, "stop-timeout");
        fs::create_dir(b.dir(&id)).unwrap();
        fs::create_dir(b.group(&id)).unwrap();
        fs::write(b.group(&id).join("cgroup.events"), "populated 1\n").unwrap();
        let mut request = ExecutionRequest::process("/bin/sh", vec![]);
        request.limits.stop_ms = 30;
        let mut row = Record {
            plan: b.plan(&id).unwrap(),
            preparing: false,
            request,
            observation: Observation {
                execution: ExecutionRef {
                    owner,
                    id: "stop-timeout".into(),
                    runtime_version: b.version.clone(),
                },
                state: ExecutionState::Running,
                result: None,
                failure: None,
                cost_micro_usd: Some(0),
                released_resources: Default::default(),
            },
            usage: None,
            group_inode: Some(fs::metadata(b.group(&id)).unwrap().ino()),
            scratch_mount_id: None,
        };
        b.save(&id, &row).unwrap();
        let lock = b.lock(&id).unwrap().unwrap();
        let started = std::time::Instant::now();
        assert!(b.finish(&id, &mut row, None, lock).await.is_err());
        assert!(
            started.elapsed() < Duration::from_millis(800),
            "must use request stop_ms, not fixed one-second polling"
        );
        let b = fixture(root.path(), groups.path());
        let mut saved = b.load(&id).unwrap();
        assert_eq!(saved.observation.state, ExecutionState::Unknown);
        assert!(
            saved
                .observation
                .failure
                .as_deref()
                .unwrap()
                .starts_with("STOP_UNCONFIRMED")
        );
        assert!(saved.observation.released_resources.is_empty());
        // An I/O error during confirmation must take the same durable path.
        fs::remove_file(b.group(&id).join("cgroup.events")).unwrap();
        let lock = b.lock(&id).unwrap().unwrap();
        assert!(b.finish(&id, &mut saved, None, lock).await.is_err());
        let mut saved = b.load(&id).unwrap();
        assert_eq!(saved.observation.state, ExecutionState::Unknown);
        assert!(
            saved
                .observation
                .failure
                .as_deref()
                .unwrap()
                .contains("LINUX_IO")
        );
        assert!(saved.observation.released_resources.is_empty());
        fs::write(b.group(&id).join("cgroup.events"), "populated 1\n").unwrap();
        assert!(matches!(
            b.recover_stop(&id, &mut saved).unwrap(),
            Progress::Pending
        ));
        fs::write(b.group(&id).join("cgroup.events"), "populated 0\n").unwrap();
        for (name, value) in [
            ("cpu.stat", "usage_usec 123\n"),
            ("memory.peak", "4096"),
            ("memory.events", "oom_kill 0\n"),
            ("pids.events", "max 0\n"),
        ] {
            fs::write(b.group(&id).join(name), value).unwrap();
        }
        assert_eq!(
            b.recover_stop(&id, &mut saved).unwrap(),
            Progress::Confirmed
        );
        let saved = b.load(&id).unwrap();
        assert_eq!(saved.observation.state, ExecutionState::Stopped);
        assert_eq!(
            saved.observation.released_resources,
            [Resource::Cpu, Resource::Pid].into()
        );
        assert_eq!(
            saved.observation.failure.as_deref(),
            Some("RECOVERED_STOP_RESULT_UNKNOWN")
        );
    }
    #[tokio::test]
    async fn stop_interrupts_staging_without_publishing_early_receipts() {
        use std::os::unix::fs::MetadataExt;
        let root = tempfile::tempdir().unwrap();
        let groups = tempfile::tempdir().unwrap();
        let b = fixture(root.path(), groups.path());
        let c = ToolContext {
            recovery_proof: None,
            claim: Claim {
                run: RunHandle {
                    tenant: "t".into(),
                    id: "r".into(),
                },
                worker: "w".into(),
                generation: 1,
                lease_until: 10000,
            },
            principal: "host".into(),
            operation: "staging".into(),
            attempt: 1,
            deadline: None,
            permission: "execute".into(),
            account_binding: "host".into(),
        };
        let id = LinuxBackend::id(&c);
        fs::create_dir(b.dir(&id)).unwrap();
        fs::create_dir(b.group(&id)).unwrap();
        fs::write(b.group(&id).join("cgroup.kill"), b"untouched").unwrap();
        let row = Record {
            plan: b.plan(&id).unwrap(),
            preparing: true,
            request: ExecutionRequest::process("/bin/sh", vec![]),
            observation: Observation {
                execution: ExecutionRef {
                    owner: c.claim.run.clone(),
                    id: c.operation.clone(),
                    runtime_version: b.version.clone(),
                },
                state: ExecutionState::Acquiring,
                result: None,
                failure: None,
                cost_micro_usd: Some(0),
                released_resources: Default::default(),
            },
            usage: None,
            group_inode: Some(fs::metadata(b.group(&id)).unwrap().ino()),
            scratch_mount_id: None,
        };
        b.save(&id, &row).unwrap();
        let _acquiring = b.lock(&id).unwrap().unwrap();
        assert_eq!(b.stop(&c).await.unwrap(), Progress::Pending);
        assert_eq!(fs::read(b.group(&id).join("cgroup.kill")).unwrap(), b"1");
        let o = b.lookup(&c).await.unwrap().unwrap();
        assert_eq!(o.state, ExecutionState::Acquiring);
        assert!(o.released_resources.is_empty());
    }
    #[test]
    fn disk_capacity_is_never_rounded_above_the_requested_bytes() {
        let root = tempfile::tempdir().unwrap();
        let groups = tempfile::tempdir().unwrap();
        let mut b = fixture(root.path(), groups.path());
        b.executables
            .insert("/bin/sh".into(), (PathBuf::from("/bin/sh"), String::new()));
        let mut r = ExecutionRequest::process("/bin/sh", vec![]);
        r.limits.cpu_millis = Some(1000);
        r.limits.ram_bytes = Some(128 * 1024 * 1024);
        r.limits.pids = Some(32);
        r.limits.disk_bytes = Some(1);
        assert_eq!(
            b.supports(&r).unwrap_err(),
            Error::Invalid("LINUX_DISK_PAGE_ALIGNMENT".into())
        );
        r.limits.disk_bytes = Some(rustix::param::page_size() as u64);
        b.supports(&r).unwrap();
    }
    #[test]
    fn observation_reads_kernel_usage_dimensions_without_inventing_zero() {
        let g = tempfile::tempdir().unwrap();
        fs::write(
            g.path().join("cpu.stat"),
            "usage_usec 12345\nuser_usec 10000\n",
        )
        .unwrap();
        fs::write(g.path().join("memory.peak"), "65536\n").unwrap();
        fs::write(g.path().join("memory.events"), "oom_kill 2\n").unwrap();
        fs::write(g.path().join("pids.events"), "max 3\n").unwrap();
        assert_eq!(
            read_usage(g.path()).unwrap(),
            LinuxUsage {
                cpu_usec: 12345,
                memory_peak_bytes: 65536,
                memory_oom_kills: 2,
                pids_limit_events: 3
            }
        );
        fs::remove_file(g.path().join("memory.peak")).unwrap();
        assert!(read_usage(g.path()).is_err());
    }
}

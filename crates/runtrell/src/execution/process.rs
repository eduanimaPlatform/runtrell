// SPDX-License-Identifier: Apache-2.0
//! Fail-closed OS confinement. No inherited env, credentials, TTY or shared cache.
use super::*;
use std::time::Duration;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    sync::watch,
};
#[derive(Clone)]
pub(super) struct Entry {
    pub(super) request: ExecutionRequest,
    pub(super) observation: Observation,
    pub(super) scratch: PathBuf,
    pub(super) cancel: watch::Sender<bool>,
}
#[derive(Clone)]
pub struct ProcessBackend {
    root: PathBuf,
    helper: PathBuf,
    launcher: PathBuf,
    executables: BTreeMap<String, (String, String)>,
    entries: Arc<Mutex<BTreeMap<String, Entry>>>,
    version: String,
}
fn io(e: std::io::Error) -> Error {
    Error::Invalid(format!("PROCESS_IO: {e}"))
}
impl ProcessBackend {
    /// All paths are trusted host configuration. root must be private and pre-created.
    /// helper is the matching runtrell-exec-limit binary; launcher is sandbox-exec/bwrap.
    pub fn new(
        root: impl AsRef<Path>,
        helper: impl AsRef<Path>,
        launcher: impl AsRef<Path>,
        allowed_executables: &[&str],
    ) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        if std::fs::canonicalize(&root).map_err(io)? != root {
            return Err(Error::PermissionDenied);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let m = std::fs::metadata(&root).map_err(io)?;
            if !m.is_dir() || m.mode() & 0o077 != 0 || m.uid() != rustix::process::getuid().as_raw()
            {
                return Err(Error::PermissionDenied);
            }
        }
        let helper = std::fs::canonicalize(helper).map_err(io)?;
        let launcher = std::fs::canonicalize(launcher).map_err(io)?;
        let mut executables = BTreeMap::new();
        for path in allowed_executables {
            let canonical = std::fs::canonicalize(path).map_err(io)?;
            let hash = crate::data::content_digest(&std::fs::read(&canonical).map_err(io)?);
            executables.insert(
                (*path).to_string(),
                (canonical.to_string_lossy().into_owned(), hash),
            );
        }
        let version = format!(
            "runtrell-process-1:{}",
            digest(&json!([
                crate::data::content_digest(&std::fs::read(&helper).map_err(io)?),
                crate::data::content_digest(&std::fs::read(&launcher).map_err(io)?),
                executables
            ]))
        );
        Ok(Self {
            root,
            helper,
            launcher,
            executables,
            entries: Arc::default(),
            version,
        })
    }
    fn entry_key(c: &ToolContext) -> String {
        key(&c.claim.run, &c.operation)
    }
    fn reject_start(&self, c: &ToolContext, error: &Error) -> Result<()> {
        let mut entries = self.entries.lock().map_err(|_| Error::Storage)?;
        if let Some(entry) = entries.get_mut(&Self::entry_key(c))
            && entry.observation.state == ExecutionState::Ready
        {
            entry.observation.state = ExecutionState::Stopped;
            entry.observation.failure = Some(format!("START_REJECTED: {error}"));
            entry.observation.local_stop();
        }
        Ok(())
    }
    fn command(&self, e: &Entry) -> Result<Command> {
        let Program::Process { executable, args } = &e.request.program;
        let l = &e.request.limits;
        let (executable, expected_hash) = self
            .executables
            .get(executable)
            .ok_or(Error::PermissionDenied)?;
        if e.request
            .executable_hash
            .as_ref()
            .is_some_and(|h| h != expected_hash)
            || crate::data::content_digest(&std::fs::read(executable).map_err(io)?)
                != *expected_hash
        {
            return Err(Error::Conflict);
        }
        let mut command = Command::new(&self.helper);
        command
            .arg(
                l.cpu_millis
                    .map(|n| n.div_ceil(1000))
                    .unwrap_or(u64::MAX)
                    .to_string(),
            )
            .arg(l.file_bytes.unwrap_or(u64::MAX).to_string())
            .arg(&self.launcher);
        #[cfg(target_os = "macos")]
        {
            fn quote(p: &str) -> Result<String> {
                if !p.is_ascii() || p.bytes().any(|b| b < 32 || b == b'"' || b == b'\\') {
                    return Err(Error::Invalid("SANDBOX_PATH".into()));
                }
                Ok(format!("\"{p}\""))
            }
            let scratch = quote(e.scratch.to_str().ok_or(Error::PermissionDenied)?)?;
            let executable = quote(executable)?;
            // process-fork remains denied by default. exec does not grant filesystem read.
            let profile = format!(
                "(version 1)(deny default)(allow process-exec)(allow sysctl-read (sysctl-name \"hw.ncpu\") (sysctl-name \"hw.activecpu\") (sysctl-name \"hw.pagesize\") (sysctl-name \"hw.memsize\") (sysctl-name \"hw.cputype\") (sysctl-name \"hw.cpusubtype\") (sysctl-name \"kern.osrelease\") (sysctl-name \"kern.ostype\") (sysctl-name \"kern.osversion\") (sysctl-name \"kern.version\") (sysctl-name \"kern.argmax\"))(allow file-read* (literal \"/\") (subpath \"/usr/bin\") (subpath \"/usr/lib\") (subpath \"/bin\") (subpath \"/System/Library\") (subpath \"/Library/Apple/System/Library\") (literal \"/dev/null\") (literal {executable}) (subpath {scratch}))(allow file-write* (subpath {scratch}))(allow signal (target self))"
            );
            command.args(["-p", &profile]);
        }
        #[cfg(target_os = "linux")]
        {
            command.args([
                "--unshare-all",
                "--die-with-parent",
                "--new-session",
                "--proc",
                "/proc",
                "--dev",
                "/dev",
                "--tmpfs",
                "/tmp",
            ]);
            for path in [
                "/usr/bin",
                "/usr/lib",
                "/usr/lib64",
                "/bin",
                "/lib",
                "/lib64",
                "/etc/ld.so.cache",
            ] {
                if Path::new(path).exists() {
                    command.args(["--ro-bind", path, path]);
                }
            }
            command
                .arg("--bind")
                .arg(&e.scratch)
                .arg("/workspace")
                .args([
                    "--chdir",
                    "/workspace",
                    "--setenv",
                    "HOME",
                    "/workspace",
                    "--setenv",
                    "TMPDIR",
                    "/workspace",
                ]);
            // Bind only the explicitly requested executable when outside runtime mounts.
            if !["/usr/", "/bin/", "/lib/", "/lib64/"]
                .iter()
                .any(|p| executable.starts_with(p))
            {
                command.args(["--ro-bind", executable, executable]);
            }
            command.arg("--");
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        return Err(Error::Unsupported("OS confinement".into()));
        command
            .arg(executable)
            .args(args)
            .env_clear()
            .env("HOME", &e.scratch)
            .env("TMPDIR", &e.scratch)
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C")
            .current_dir(&e.scratch)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        Ok(command)
    }
}
impl ExecutionBackend for ProcessBackend {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            isolation: Isolation::Untrusted,
            cpu: cfg!(target_os = "macos"),
            ram: false,
            pids: cfg!(target_os = "macos"),
            disk: false,
            file: cfg!(target_os = "linux"),
            network_deny: cfg!(any(target_os = "linux", target_os = "macos")),
            runtime_version: self.version.clone(),
        }
    }
    fn supports(&self, r: &ExecutionRequest) -> Result<()> {
        self.capabilities().check(r)?;
        if cfg!(target_os = "macos") && r.requires_descendants {
            return Err(Error::Unsupported(
                "Seatbelt backend denies fork; descendant execution unavailable".into(),
            ));
        }
        let Program::Process { executable, args } = &r.program;
        if !self.executables.contains_key(executable) {
            return Err(Error::PermissionDenied);
        }
        if !Path::new(executable).is_absolute()
            || args.len() > 256
            || args.iter().map(String::len).sum::<usize>() > 65_536
        {
            return Err(Error::Invalid("PROCESS_ARGUMENTS".into()));
        }
        if r.output_paths.len() > 256 {
            return Err(Error::Invalid("OUTPUT_PATHS".into()));
        }
        for path in &r.output_paths {
            crate::data::logical_path(path)?;
        }
        // CPU is rounded to OS seconds: do not silently promise a subsecond hard cap.
        if r.limits.cpu_millis.is_some_and(|n| n % 1000 != 0) {
            return Err(Error::Unsupported("CPU requires whole seconds".into()));
        }
        Ok(())
    }
    async fn acquire(&self, c: &ToolContext, r: &ExecutionRequest) -> Result<ExecutionRef> {
        self.supports(r)?;
        r.validate_owner(c)?;
        let id = Self::entry_key(c);
        let (execution, scratch) = {
            let mut entries = self.entries.lock().map_err(|_| Error::Storage)?;
            if let Some(old) = entries.get(&id) {
                if old.request != *r {
                    return Err(Error::Conflict);
                }
                return Ok(old.observation.execution.clone());
            }
            let scratch = self.root.join(&id);
            let execution = ExecutionRef {
                owner: c.claim.run.clone(),
                id: c.operation.clone(),
                runtime_version: self.version.clone(),
            };
            let entry = Entry {
                request: r.clone(),
                observation: Observation {
                    execution: execution.clone(),
                    state: ExecutionState::Acquiring,
                    result: None,
                    failure: None,
                    cost_micro_usd: Some(0),
                    released_resources: Default::default(),
                },
                scratch: scratch.clone(),
                cancel: watch::channel(false).0,
            };
            entries.insert(id.clone(), entry);
            (execution, scratch)
        };
        let backend = self.clone();
        let r = r.clone();
        tokio::task::spawn_blocking(move || {
            // A pre-existing directory is quarantined; never adopt or delete it.
            if let Err(error) = std::fs::create_dir(&scratch) {
                backend
                    .entries
                    .lock()
                    .map_err(|_| Error::Storage)?
                    .get_mut(&id)
                    .ok_or(Error::NotFound)?
                    .observation
                    .state = ExecutionState::Unknown;
                return Err(io(error));
            }
            let copied = (|| -> Result<()> {
                for (path, bytes) in &r.input_files {
                    crate::data::logical_path(path)?;
                    let path = scratch.join(path);
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent).map_err(io)?;
                    }
                    std::fs::write(path, bytes).map_err(io)?;
                }
                Ok(())
            })();
            let mut entries = backend.entries.lock().map_err(|_| Error::Storage)?;
            let entry = entries.get_mut(&id).ok_or(Error::NotFound)?;
            match copied {
                Ok(()) => {
                    entry.observation.state = if *entry.cancel.borrow() {
                        ExecutionState::Stopped
                    } else {
                        ExecutionState::Ready
                    };
                    if entry.observation.state == ExecutionState::Stopped {
                        entry.observation.failure = Some("CANCELLED".into());
                        entry.observation.local_stop();
                    }
                }
                Err(err) => {
                    entry.observation.state = ExecutionState::Stopped;
                    entry.observation.local_stop();
                    entry.observation.failure = Some(err.to_string());
                    return Err(err);
                }
            }
            Ok(execution)
        })
        .await
        .map_err(|_| Error::Storage)?
    }
    async fn start(&self, c: &ToolContext, execution: &ExecutionRef) -> Result<()> {
        let id = Self::entry_key(c);
        let mut job = {
            let mut entries = self.entries.lock().map_err(|_| Error::Storage)?;
            let entry = entries.get_mut(&id).ok_or(Error::NotFound)?;
            if entry.observation.execution != *execution {
                return Err(Error::PermissionDenied);
            }
            if *entry.cancel.borrow() {
                return Err(Error::Closed);
            }
            if entry.observation.state != ExecutionState::Ready {
                return Ok(());
            }
            entry.clone()
        };
        let backend = self.clone();
        let config = job.clone();
        let preparation = tokio::task::spawn_blocking(move || backend.command(&config))
            .await
            .map_err(|_| Error::Storage)
            .and_then(|r| r);
        let command = match preparation {
            Ok(command) => command,
            Err(error) => {
                self.reject_start(c, &error)?;
                return Err(error);
            }
        };
        job.request = match with_deadline(job.request, c) {
            Ok(request) => request,
            Err(error) => {
                self.reject_start(c, &error)?;
                return Err(error);
            }
        };
        let mut entries = self.entries.lock().map_err(|_| Error::Storage)?;
        let entry = entries.get_mut(&id).ok_or(Error::NotFound)?;
        if *entry.cancel.borrow() {
            return Err(Error::Closed);
        }
        if entry.observation.state != ExecutionState::Ready {
            return Ok(());
        }
        entry.observation.state = ExecutionState::Running;
        let entries = self.entries.clone();
        // Supervisor outlives the caller's future; it always kill/waits before StopConfirmed.
        tokio::spawn(async move {
            let (mut result, mut failure) = supervise(command, &job).await;
            if !failure
                .as_ref()
                .is_some_and(|s| s.starts_with("STOP_UNCONFIRMED"))
                && let Ok(mut rows) = entries.lock()
                && let Some(e) = rows.get_mut(&id)
            {
                e.observation.state = ExecutionState::Collecting;
                e.observation.local_stop();
            }
            tokio::task::spawn_blocking(move || {
                if failure.is_none() {
                    match collect_files(&job) {
                        Ok(files) => {
                            if let Some(v) = &mut result {
                                v["files"] = json!(files);
                            }
                        }
                        Err(err) => failure = Some(err.to_string()),
                    }
                }
                if result.as_ref().is_some_and(|v| {
                    serde_json::to_vec(v)
                        .map_or(true, |bytes| bytes.len() > job.request.limits.output_bytes)
                }) {
                    result = None;
                    failure = Some("OUTPUT_LIMIT".into());
                }
                if let Ok(mut rows) = entries.lock()
                    && let Some(e) = rows.get_mut(&id)
                {
                    e.observation.result = result;
                    e.observation.failure = failure;
                    e.observation.state = if e
                        .observation
                        .failure
                        .as_ref()
                        .is_some_and(|s| s.starts_with("STOP_UNCONFIRMED"))
                    {
                        ExecutionState::Unknown
                    } else {
                        ExecutionState::Stopped
                    };
                }
            });
        });
        Ok(())
    }
    async fn lookup(&self, c: &ToolContext) -> Result<Option<Observation>> {
        Ok(self
            .entries
            .lock()
            .map_err(|_| Error::Storage)?
            .get(&Self::entry_key(c))
            .map(|e| e.observation.clone()))
    }
    async fn cancel(&self, c: &ToolContext) -> Result<()> {
        let mut entries = self.entries.lock().map_err(|_| Error::Storage)?;
        if let Some(e) = entries.get_mut(&Self::entry_key(c)) {
            e.cancel.send_replace(true);
            if e.observation.state == ExecutionState::Ready {
                e.observation.state = ExecutionState::Stopped;
                e.observation.failure = Some("CANCELLED".into());
                e.observation.local_stop();
            }
        }
        Ok(())
    }
    async fn stop(&self, c: &ToolContext) -> Result<Progress> {
        self.cancel(c).await?;
        Ok(match self.lookup(c).await? {
            Some(o)
                if matches!(
                    o.state,
                    ExecutionState::Collecting
                        | ExecutionState::Stopped
                        | ExecutionState::Reclaiming
                        | ExecutionState::Destroyed
                ) =>
            {
                Progress::Confirmed
            }
            Some(o) if o.state == ExecutionState::Unknown => Progress::Unknown,
            Some(_) => Progress::Pending,
            None => Progress::Unknown,
        })
    }
    async fn destroy(&self, c: &ToolContext) -> Result<Progress> {
        let scratch = {
            let mut entries = self.entries.lock().map_err(|_| Error::Storage)?;
            let Some(e) = entries.get_mut(&Self::entry_key(c)) else {
                return Ok(Progress::Unknown);
            };
            if e.observation.state == ExecutionState::Destroyed {
                return Ok(Progress::Confirmed);
            }
            if e.observation.state == ExecutionState::Unknown {
                return Ok(Progress::Unknown);
            }
            if e.observation.state != ExecutionState::Stopped {
                return Ok(Progress::Pending);
            }
            let scratch = e.scratch.clone();
            e.observation.state = ExecutionState::Reclaiming;
            scratch
        };
        let id = Self::entry_key(c);
        let backend = self.clone();
        tokio::task::spawn_blocking(move || {
            let result = std::fs::remove_dir_all(&scratch);
            let mut entries = backend.entries.lock().map_err(|_| Error::Storage)?;
            let e = entries.get_mut(&id).ok_or(Error::NotFound)?;
            match result {
                Ok(()) => (),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => (),
                Err(err) => {
                    e.observation.state = ExecutionState::Stopped;
                    return Err(io(err));
                }
            }
            e.observation.state = ExecutionState::Destroyed;
            e.observation.local_destroy();
            Ok(Progress::Confirmed)
        })
        .await
        .map_err(|_| Error::Storage)?
    }
}
pub(super) async fn bounded(
    mut stream: impl AsyncRead + Unpin,
    limit: usize,
) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            return Ok(out);
        }
        if n > limit.saturating_sub(out.len()) {
            return Err(std::io::Error::other("OUTPUT_LIMIT"));
        }
        out.extend_from_slice(&buf[..n]);
    }
}
async fn supervise(mut command: Command, job: &Entry) -> (Option<Value>, Option<String>) {
    let mut cancel = job.cancel.subscribe();
    if *cancel.borrow() {
        return (None, Some("CANCELLED".into()));
    }
    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => return (None, Some(format!("START_FAILED: {e}"))),
    };
    let out = child.stdout.take().expect("piped stdout");
    let err = child.stderr.take().expect("piped stderr");
    // stdout, stderr and collected files each receive a third of the aggregate budget.
    let limit = job.request.limits.output_bytes / 3;
    let result = tokio::select! {
        _=cancel.changed()=>Err("CANCELLED".into()),
        _=tokio::time::sleep(Duration::from_millis(job.request.limits.wall_ms))=>Err("DEADLINE_EXCEEDED".into()),
        result=async {let (out,err,status)=tokio::try_join!(bounded(out,limit),bounded(err,limit),child.wait())?;
            Ok::<_,std::io::Error>((out,err,status))}=>result.map_err(|e|e.to_string()),
    };
    // Even a successful leader exit must not leave descendants. bwrap is kept alive until
    // namespace init exits; Seatbelt denies fork. kill/wait errors must remain Unknown.
    if result.is_err() {
        let _ = child.start_kill();
        if let Err(e) = child.wait().await {
            return (None, Some(format!("STOP_UNCONFIRMED: {e}")));
        }
    }
    match result {
        Ok((out, err, status)) => {
            let value = json!({"stdout":out,"stderr":err,"exit":status.code()});
            if status.success() {
                (Some(value), None)
            } else {
                (Some(value), Some(format!("PROCESS_EXIT: {status}")))
            }
        }
        Err(e) => (None, Some(e)),
    }
}

pub(super) fn collect_files(job: &Entry) -> Result<BTreeMap<String, Option<Vec<u8>>>> {
    use rustix::fs::{Mode, OFlags, open, openat};
    use std::{
        collections::BTreeSet,
        io::{Read, Seek},
    };
    let root = open(
        &job.scratch,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| Error::PermissionDenied)?;
    let mut result = BTreeMap::new();
    let mut remaining = job.request.limits.output_bytes / 3;
    let paths: BTreeSet<_> = job
        .request
        .input_files
        .keys()
        .chain(job.request.output_paths.iter())
        .collect();
    for path in paths {
        crate::data::logical_path(path)?;
        let parts = path.split('/').collect::<Vec<_>>();
        let mut directory = rustix::io::dup(&root).map_err(|_| Error::Storage)?;
        let mut absent = false;
        for part in &parts[..parts.len() - 1] {
            match openat(
                &directory,
                *part,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            ) {
                Ok(fd) => directory = fd,
                Err(rustix::io::Errno::NOENT) => {
                    absent = true;
                    break;
                }
                Err(_) => return Err(Error::PermissionDenied),
            }
        }
        let file = if absent {
            None
        } else {
            match openat(
                &directory,
                *parts.last().ok_or(Error::PermissionDenied)?,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            ) {
                Ok(fd) => Some(std::fs::File::from(fd)),
                Err(rustix::io::Errno::NOENT) => None,
                Err(_) => return Err(Error::PermissionDenied),
            }
        };
        if let Some(mut file) = file {
            let metadata = file.metadata().map_err(io)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if metadata.nlink() != 1 {
                    return Err(Error::PermissionDenied);
                }
            }
            if !metadata.is_file() {
                return Err(Error::PermissionDenied);
            }
            if !job.request.output_paths.contains(path)
                && let Some(original) = job.request.input_files.get(path)
                && metadata.len() == original.len() as u64
            {
                let mut offset = 0usize;
                let mut unchanged = true;
                let mut buffer = [0u8; 4096];
                loop {
                    let n = file.read(&mut buffer).map_err(io)?;
                    if n == 0 {
                        break;
                    }
                    if original.get(offset..offset + n) != Some(&buffer[..n]) {
                        unchanged = false;
                        break;
                    }
                    offset += n;
                }
                if unchanged && offset == original.len() {
                    continue;
                }
                file.rewind().map_err(io)?;
            }
            remaining = remaining
                .checked_sub(path.len().saturating_add(16))
                .ok_or(Error::Invalid("OUTPUT_FILE_LIMIT".into()))?;
            if metadata.len() > remaining as u64 {
                return Err(Error::Invalid("OUTPUT_FILE_LIMIT".into()));
            }
            let mut bytes = Vec::new();
            file.take(remaining as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(io)?;
            if bytes.len() > remaining {
                return Err(Error::Invalid("OUTPUT_FILE_LIMIT".into()));
            }
            remaining -= bytes.len();
            result.insert(path.clone(), Some(bytes));
        } else if job.request.input_files.contains_key(path) {
            remaining = remaining
                .checked_sub(path.len().saturating_add(16))
                .ok_or(Error::Invalid("OUTPUT_FILE_LIMIT".into()))?;
            result.insert(path.clone(), None);
        }
    }
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn collection_bounds_data_and_refuses_links_and_special_files() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let mut request = ExecutionRequest::process("/bin/sh", vec![]);
        request.output_paths = vec!["out".into()];
        let e = Entry {
            request,
            observation: Observation {
                execution: ExecutionRef {
                    owner: RunHandle {
                        tenant: "t".into(),
                        id: "r".into(),
                    },
                    id: "op".into(),
                    runtime_version: "test".into(),
                },
                state: ExecutionState::Stopped,
                result: None,
                failure: None,
                cost_micro_usd: Some(0),
                released_resources: Default::default(),
            },
            scratch: root.path().to_path_buf(),
            cancel: watch::channel(false).0,
        };
        std::fs::write(root.path().join("out"), b"bounded").unwrap();
        assert_eq!(collect_files(&e).unwrap()["out"], Some(b"bounded".to_vec()));
        std::fs::write(root.path().join("out"), vec![0; 2000]).unwrap();
        assert!(collect_files(&e).is_err());
        std::fs::remove_file(root.path().join("out")).unwrap();
        symlink("/etc/passwd", root.path().join("out")).unwrap();
        assert!(collect_files(&e).is_err());
        std::fs::remove_file(root.path().join("out")).unwrap();
        std::fs::write(root.path().join("other"), b"data").unwrap();
        std::fs::hard_link(root.path().join("other"), root.path().join("out")).unwrap();
        assert!(collect_files(&e).is_err());
    }
}

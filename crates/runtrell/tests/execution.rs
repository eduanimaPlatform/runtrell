// SPDX-License-Identifier: Apache-2.0
use runtrell::{execution::*, *};
fn context() -> ToolContext {
    ToolContext {
        recovery_proof: None,
        claim: Claim {
            run: RunHandle {
                tenant: "t".into(),
                id: "r".into(),
            },
            worker: "w".into(),
            generation: 1,
            lease_until: 100000,
        },
        principal: "host".into(),
        operation: "op".into(),
        attempt: 1,
        deadline: None,
        permission: "execute".into(),
        account_binding: "host".into(),
    }
}
#[tokio::test]
async fn a27_environment_is_private_and_failed_isolation_never_runs_payload() {
    let root = private_root();
    let marker = root.path().join("escaped");
    let helper = env!("CARGO_BIN_EXE_runtrell-exec-limit");
    // A launcher that fails before exec is an injected isolation failure, not a fallback.
    let b = ProcessBackend::new(
        root.path().canonicalize().unwrap(),
        helper,
        "/usr/bin/false",
        &["/bin/sh"],
    )
    .unwrap();
    let mut r = ExecutionRequest::process("/bin/sh", vec![]);
    r.program = Program::Process {
        executable: "/bin/sh".into(),
        args: vec![
            "-c".into(),
            format!("printf escaped > {}", marker.display()),
        ],
    };
    r.limits.isolation = Isolation::Untrusted;
    let c = context();
    let e = b.acquire(&c, &r).await.unwrap();
    b.start(&c, &e).await.unwrap();
    for _ in 0..200 {
        if b.lookup(&c).await.unwrap().unwrap().state == ExecutionState::Stopped {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let o = b.lookup(&c).await.unwrap().unwrap();
    assert_eq!(o.state, ExecutionState::Stopped);
    assert!(o.failure.is_some());
    assert!(!marker.exists());
    assert_eq!(b.destroy(&c).await.unwrap(), Progress::Confirmed);
    assert_eq!(b.destroy(&c).await.unwrap(), Progress::Confirmed);
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}
#[test]
fn hard_resource_requirements_are_rejected_before_acquisition() {
    let root = private_root();
    let b = ProcessBackend::new(
        root.path().canonicalize().unwrap(),
        env!("CARGO_BIN_EXE_runtrell-exec-limit"),
        "/usr/bin/false",
        &["/bin/sh"],
    )
    .unwrap();
    let mut r = ExecutionRequest::process("/bin/sh", vec![]);
    r.program = Program::Process {
        executable: "/bin/sh".into(),
        args: vec![],
    };
    r.limits.ram_bytes = Some(1);
    assert!(b.supports(&r).is_err());
    r.limits.ram_bytes = None;
    r.limits.disk_bytes = Some(1);
    assert!(b.supports(&r).is_err());
    r.limits.disk_bytes = None;
    #[cfg(target_os = "linux")]
    {
        r.limits.pids = Some(2);
        assert!(b.supports(&r).is_err());
        r.limits.pids = None;
        r.limits.cpu_millis = Some(1000);
        assert!(b.supports(&r).is_err());
    }
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}
#[tokio::test]
#[ignore = "requires usable Seatbelt or bubblewrap namespaces; run explicitly on a confinement-capable host"]
async fn real_isolation_loop_output_fork_env_and_workspace_boundaries() {
    let root = private_root();
    #[cfg(target_os = "macos")]
    let launcher = "/usr/bin/sandbox-exec";
    #[cfg(target_os = "linux")]
    let launcher = "/usr/bin/bwrap";
    let b = ProcessBackend::new(
        root.path().canonicalize().unwrap(),
        env!("CARGO_BIN_EXE_runtrell-exec-limit"),
        launcher,
        &["/bin/sh"],
    )
    .unwrap();
    let probes = [
        ("ok", "printf '%s' \"$HOME|$TMPDIR|$PATH\"", false),
        ("loop", "while :; do :; done", true),
        (
            "fork",
            "/bin/sleep 10 & printf '%s' \"$!\" > child.pid; wait",
            true,
        ),
        (
            "output",
            "while :; do printf 'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx'; done",
            true,
        ),
        (
            "escape",
            "printf forbidden > /usr/runtrell-forbidden-write",
            true,
        ),
    ];
    for (id, script, failure) in probes {
        let mut c = context();
        c.operation = id.into();
        let mut r = ExecutionRequest::process("/bin/sh", vec![]);
        r.program = Program::Process {
            executable: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
        };
        r.limits.isolation = Isolation::Untrusted;
        r.limits.wall_ms = if id == "ok" {
            2000
        } else if id == "fork" {
            1000
        } else {
            200
        };
        if id == "fork" {
            r.requires_descendants = true;
            #[cfg(target_os = "macos")]
            {
                assert!(
                    matches!(b.acquire(&c, &r).await, Err(Error::Unsupported(_))),
                    "macOS must reject descendant execution before acquisition"
                );
                assert!(b.lookup(&c).await.unwrap().is_none());
                continue;
            }
        }
        let e = b.acquire(&c, &r).await.unwrap();
        let started = std::time::Instant::now();
        b.start(&c, &e).await.unwrap();
        loop {
            let o = b.lookup(&c).await.unwrap().unwrap();
            if o.state == ExecutionState::Stopped {
                assert_eq!(o.failure.is_some(), failure, "{id}: {o:?}");
                break;
            }
            assert!(started.elapsed().as_secs() < 3, "stop bound exceeded");
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        if id == "fork" {
            let path = root
                .path()
                .join(digest(&serde_json::json!([c.claim.run, c.operation])))
                .join("child.pid");
            assert!(path.exists(), "fixture must actually create a child");
            let pid = std::fs::read_to_string(path)
                .expect("child PID file")
                .parse::<i32>()
                .expect("valid numeric PID");
            assert!(pid > 1, "valid child PID");
            let pid = rustix::process::Pid::from_raw(pid).expect("valid child PID");
            {
                let until = std::time::Instant::now();
                while rustix::process::test_kill_process(pid).is_ok()
                    && until.elapsed().as_millis() < 500
                {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
                assert!(
                    rustix::process::test_kill_process(pid).is_err(),
                    "descendant must be gone before cleanup"
                );
            }
        }
        assert_eq!(b.destroy(&c).await.unwrap(), Progress::Confirmed);
    }
}

fn private_root() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    root
}
#[tokio::test]
async fn helper_applies_rlimits_and_cpu_stops_a_trusted_busy_fixture() {
    let root = private_root();
    let mut limits = tokio::process::Command::new(env!("CARGO_BIN_EXE_runtrell-exec-limit"));
    limits
        .args([
            "1",
            "4096",
            "/bin/bash",
            "-c",
            "ulimit -Ht; ulimit -Hf; ulimit -Hn",
        ])
        .env_clear()
        .current_dir(root.path());
    let out = limits.output().await.unwrap();
    assert!(
        out.status.success(),
        "child stdout: {}\nchild stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "1\n4\n64\n");
    let mut busy = tokio::process::Command::new(env!("CARGO_BIN_EXE_runtrell-exec-limit"));
    busy.args(["1", "4096", "/bin/bash", "-c", "while :; do :; done"])
        .env_clear()
        .current_dir(root.path())
        .kill_on_drop(true);
    busy.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = busy.spawn().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let capture = tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut out = Vec::new();
        let mut err = Vec::new();
        stdout.read_to_end(&mut out).await.unwrap();
        stderr.read_to_end(&mut err).await.unwrap();
        (out, err)
    });
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await;
    if result.is_err() {
        child.kill().await.unwrap();
        child.wait().await.unwrap();
    }
    let (out, err) = capture.await.unwrap();
    assert!(
        result.is_ok(),
        "CPU probe did not stop within wall watchdog; child stdout: {}\nchild stderr: {}",
        String::from_utf8_lossy(&out),
        String::from_utf8_lossy(&err)
    );
    assert!(
        !result.unwrap().unwrap().success(),
        "CPU probe unexpectedly succeeded; child stdout: {}\nchild stderr: {}",
        String::from_utf8_lossy(&out),
        String::from_utf8_lossy(&err)
    );
}

#[test]
fn helper_closes_inherited_non_cloexec_descriptors() {
    use rustix::io::{FdFlags, fcntl_dupfd_cloexec, fcntl_setfd};
    use std::os::fd::AsRawFd;
    let root = private_root();
    let file = std::fs::File::create(root.path().join("synthetic-credential")).unwrap();
    let inherited = fcntl_dupfd_cloexec(&file, 300).unwrap();
    fcntl_setfd(&inherited, FdFlags::empty()).unwrap();
    let script = format!(
        "if [ -e /dev/fd/{} ]; then exit 7; fi",
        inherited.as_raw_fd()
    );
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_runtrell-exec-limit"))
        .args(["1", "4096", "/bin/sh", "-c", &script])
        .env_clear()
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "inherited FD must be closed before payload exec; child stdout: {}\nchild stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
#[test]
fn destroyed_environment_does_not_release_an_unconfirmed_persistent_volume() {
    let ctx = context();
    let mut record = ExecutionRecord::intent(&ctx, ExecutionRequest::process("/bin/sh", vec![]));
    let o:Observation=serde_json::from_value(serde_json::json!({"execution":{"owner":ctx.claim.run,"id":"op","runtime_version":"external"},"state":"Destroyed","result":null,"failure":null,"cost_micro_usd":null,"released_resources":["Cpu","Memory","Pid","Disk"]})).unwrap();
    record.observe(o);
    assert!(record.resources[&Resource::Disk].released);
    assert!(!record.resources[&Resource::Volume].released);
    assert!(!record.resources[&Resource::ExternalCost].released);
}

#[tokio::test]
async fn process_registry_loss_quarantines_existing_scratch() {
    let root = private_root();
    let new_backend = || {
        ProcessBackend::new(
            root.path().canonicalize().unwrap(),
            env!("CARGO_BIN_EXE_runtrell-exec-limit"),
            "/usr/bin/false",
            &["/bin/sh"],
        )
        .unwrap()
    };
    let c = context();
    let request = ExecutionRequest::process("/bin/sh", vec![]);
    let b = new_backend();
    let original = b.acquire(&c, &request).await.unwrap();
    let scratch = std::fs::read_dir(root.path())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    std::fs::write(scratch.join("quarantined"), b"retain").unwrap();
    drop(b);
    let b = new_backend();
    assert!(b.lookup(&c).await.unwrap().is_none());
    assert_eq!(b.stop(&c).await.unwrap(), Progress::Unknown);
    assert!(b.acquire(&c, &request).await.is_err());
    assert_eq!(
        b.lookup(&c).await.unwrap().unwrap().state,
        ExecutionState::Unknown
    );
    assert_eq!(b.destroy(&c).await.unwrap(), Progress::Unknown);
    let mut foreign = c.clone();
    foreign.claim.run.id = "other".into();
    assert!(b.start(&foreign, &original).await.is_err());
    assert!(b.lookup(&foreign).await.unwrap().is_none());
    assert_eq!(
        std::fs::read(scratch.join("quarantined")).unwrap(),
        b"retain"
    );
}

#[tokio::test]
async fn changed_runtime_is_rejected_and_its_unused_environment_is_reclaimable() {
    let root = private_root();
    let executable = root.path().join("synthetic-runtime");
    std::fs::write(&executable, b"version-one").unwrap();
    let executable = executable.to_str().unwrap();
    let b = ProcessBackend::new(
        root.path().canonicalize().unwrap(),
        env!("CARGO_BIN_EXE_runtrell-exec-limit"),
        "/usr/bin/false",
        &[executable],
    )
    .unwrap();
    let c = context();
    let mut request = ExecutionRequest::process("/bin/sh", vec![]);
    request.program = Program::Process {
        executable: executable.into(),
        args: vec![],
    };
    let e = b.acquire(&c, &request).await.unwrap();
    std::fs::write(executable, b"version-two").unwrap();
    assert_eq!(b.start(&c, &e).await.unwrap_err(), Error::Conflict);
    assert_eq!(
        b.lookup(&c).await.unwrap().unwrap().state,
        ExecutionState::Stopped
    );
    assert_eq!(b.destroy(&c).await.unwrap(), Progress::Confirmed);
}

#[tokio::test]
async fn request_runtime_hash_is_not_replaced_by_a_new_backend_allowlist() {
    let root = private_root();
    let runtime = root.path().join("pinned-runtime");
    std::fs::write(&runtime, b"expected").unwrap();
    let mut request = ExecutionRequest::process(runtime.to_str().unwrap(), vec![]);
    request.executable_hash = Some(runtrell::data::content_digest(b"expected"));
    std::fs::write(&runtime, b"replacement").unwrap();
    let backend = ProcessBackend::new(
        root.path().canonicalize().unwrap(),
        env!("CARGO_BIN_EXE_runtrell-exec-limit"),
        "/usr/bin/false",
        &[runtime.to_str().unwrap()],
    )
    .unwrap();
    let c = context();
    let e = backend.acquire(&c, &request).await.unwrap();
    assert_eq!(backend.start(&c, &e).await.unwrap_err(), Error::Conflict);
    assert_eq!(backend.stop(&c).await.unwrap(), Progress::Confirmed);
    assert_eq!(backend.destroy(&c).await.unwrap(), Progress::Confirmed);
}

#[cfg(target_os = "macos")]
#[path = "support/execution_tool.rs"]
mod execution_tool;
#[tokio::test]
#[ignore = "requires macOS Seatbelt outside a nested sandbox"]
#[cfg(target_os = "macos")]
async fn macos_execution_tool_saves_and_reclaims() {
    let root = private_root();
    let backend = ProcessBackend::new(
        root.path().canonicalize().unwrap(),
        env!("CARGO_BIN_EXE_runtrell-exec-limit"),
        "/usr/bin/sandbox-exec",
        &["/bin/sh"],
    )
    .unwrap();
    let limits = ExecutionLimits {
        isolation: Isolation::Untrusted,
        wall_ms: 4000,
        output_bytes: 8192,
        ..Default::default()
    };
    execution_tool::exercise(backend, limits).await;
}

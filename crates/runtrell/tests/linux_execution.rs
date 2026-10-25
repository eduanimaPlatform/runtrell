// SPDX-License-Identifier: Apache-2.0
// Type-check the privileged fixtures on Unix hosts; execute them only on Linux.
#![cfg(unix)]
use runtrell::{execution::*, *};
fn context(id: &str) -> ToolContext {
    ToolContext {
        recovery_proof: None,
        claim: Claim {
            run: RunHandle {
                tenant: "linux-test".into(),
                id: "run".into(),
            },
            worker: "worker".into(),
            generation: 1,
            lease_until: u64::MAX,
        },
        principal: "host".into(),
        operation: id.into(),
        attempt: 1,
        deadline: None,
        permission: "execute".into(),
        account_binding: "test".into(),
    }
}
fn backend(root: &std::path::Path) -> LinuxBackend {
    assert_eq!(
        std::env::var("RUNTRELL_LINUX_TEST").as_deref(),
        Ok("1"),
        "explicit privileged test opt-in required"
    );
    LinuxBackend::new(
        root,
        std::env::var("RUNTRELL_CGROUP_ROOT").expect("delegated cgroup v2 root"),
        env!("CARGO_BIN_EXE_runtrell-exec-limit"),
        "/usr/bin/bwrap",
        &["/bin/sh", "/usr/bin/python3"],
        LinuxCpuBandwidth {
            quota_usec: 10000,
            period_usec: 10000,
        },
    )
    .unwrap()
}
fn request(script: &str) -> ExecutionRequest {
    let mut r = ExecutionRequest::process("/bin/sh", vec!["-c".into(), script.into()]);
    r.limits.cpu_millis = Some(1000);
    r.limits.ram_bytes = Some(128 * 1024 * 1024);
    r.limits.pids = Some(32);
    r.limits.disk_bytes = Some(4 * 1024 * 1024);
    r.limits.wall_ms = 4000;
    r.limits.output_bytes = 8192;
    r
}
async fn stopped(b: &LinuxBackend, c: &ToolContext) -> Observation {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let o = b.lookup(c).await.unwrap().unwrap();
            if o.state == ExecutionState::Stopped {
                assert!(o.released_resources.contains(&Resource::Cpu));
                assert!(o.released_resources.contains(&Resource::Pid));
                assert!(
                    !o.released_resources.contains(&Resource::Memory),
                    "tmpfs still retains memory after process stop"
                );
                assert!(!o.released_resources.contains(&Resource::Disk));
                return o;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("bounded stop")
}
async fn child_started(root: &std::path::Path, c: &ToolContext) {
    let id = digest(&serde_json::json!([c.claim.run, c.operation]));
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !root.join(&id).join("scratch/child-started").exists() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("fixture must actually create a child before cancellation/crash");
}
async fn destroyed(b: &LinuxBackend, c: &ToolContext) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match b.destroy(c).await.unwrap() {
                Progress::Confirmed => break,
                Progress::Pending => tokio::time::sleep(std::time::Duration::from_millis(5)).await,
                Progress::Unknown | Progress::NotRequired => {
                    panic!("cleanup has no deletion receipt")
                }
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(b.destroy(c).await.unwrap(), Progress::Confirmed);
    let o = b.lookup(c).await.unwrap().unwrap();
    assert!(o.released_resources.contains(&Resource::Memory));
    assert!(o.released_resources.contains(&Resource::Disk));
}
struct StateRoot(std::path::PathBuf);
impl StateRoot {
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}
fn root() -> StateRoot {
    use std::os::unix::fs::PermissionsExt;
    let parent =
        std::env::var("RUNTRELL_LINUX_STATE_ROOT").expect("private durable test state directory");
    let path = tempfile::Builder::new()
        .prefix("linux-")
        .tempdir_in(parent)
        .unwrap()
        .keep();
    let d = StateRoot(path);
    std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    d
}
#[tokio::test]
#[ignore = "requires RUNTRELL_LINUX_TEST=1, delegated cgroup v2, CAP_SYS_ADMIN and bubblewrap"]
async fn linux_limits_and_boundaries() {
    let root = root();
    let b = backend(root.path());
    let c = context("baseline");
    let r = request("printf ready");
    let e = b.acquire(&c, &r).await.unwrap();
    b.start(&c, &e).await.unwrap();
    let o = stopped(&b, &c).await;
    assert!(
        o.failure.is_none(),
        "confinement must actually execute: {o:?}"
    );
    assert_eq!(
        serde_json::from_value::<Vec<u8>>(o.result.unwrap()["stdout"].clone()).unwrap(),
        b"ready"
    );
    destroyed(&b, &c).await;
    for (id, script) in [
        ("cpu", "while :; do :; done"),
        ("ram", "/usr/bin/python3 -c 'a=bytearray(256*1024*1024)'"),
        (
            "pids",
            "/usr/bin/python3 -c 'import os,time; [os.fork() for _ in range(8)]; time.sleep(30)'",
        ),
        ("disk", "dd if=/dev/zero of=large bs=1048576 count=16"),
        (
            "output",
            "while :; do printf xxxxxxxxxxxxxxxxxxxxxxxxx; done",
        ),
        ("escape", "echo forbidden > /usr/forbidden"),
        (
            "network",
            "/usr/bin/python3 -c 'import socket; socket.create_connection((\"1.1.1.1\",53),1)'",
        ),
    ] {
        let c = context(id);
        let r = request(script);
        let e = b.acquire(&c, &r).await.unwrap();
        b.start(&c, &e).await.unwrap();
        let o = stopped(&b, &c).await;
        assert!(o.failure.is_some(), "{id}: {o:?}");
        assert!(o.released_resources.contains(&Resource::Pid));
        let usage = b.usage(&c).unwrap();
        assert!(usage.memory_peak_bytes > 0);
        if id == "cpu" {
            assert!(o.failure.as_deref().unwrap().contains("CPU_LIMIT"), "{o:?}");
            assert!(usage.cpu_usec >= 1_000_000);
        }
        if id == "ram" {
            assert!(
                usage.memory_oom_kills > 0,
                "must hit cgroup OOM, not just fail to launch"
            );
        }
        if id == "pids" {
            assert!(usage.pids_limit_events > 0, "must hit pids.max");
        }
        if id == "disk" {
            let stderr: Vec<u8> =
                serde_json::from_value(o.result.as_ref().unwrap()["stderr"].clone()).unwrap();
            assert!(String::from_utf8_lossy(&stderr).contains("No space left"));
        }
        if id == "network" || id == "escape" {
            let stderr: Vec<u8> =
                serde_json::from_value(o.result.as_ref().unwrap()["stderr"].clone()).unwrap();
            let expected = if id == "network" {
                "Network is unreachable"
            } else {
                "Read-only file system"
            };
            assert!(String::from_utf8_lossy(&stderr).contains(expected), "{o:?}");
        }
        if id == "output" {
            assert!(o.failure.as_deref().unwrap().contains("OUTPUT_LIMIT"));
        }

        destroyed(&b, &c).await;
    }
}
#[tokio::test]
#[ignore = "requires privileged Linux test environment"]
async fn linux_cancel_descendants_and_recover_without_registry() {
    let root = root();
    let b = backend(root.path());
    let c = context("recover");
    let r = request("sleep 30 & echo ready > child-started; wait");
    let ledger = MemoryExecutionLedger::default();
    ledger
        .insert(ExecutionRecord::intent(&c, r.clone()))
        .await
        .unwrap();
    let e = b.acquire(&c, &r).await.unwrap();
    b.start(&c, &e).await.unwrap();
    child_started(root.path(), &c).await;
    let fresh = backend(root.path());
    assert!(
        fresh
            .inventory()
            .unwrap()
            .iter()
            .any(|i| i.execution.as_ref() == Some(&e))
    );
    assert_eq!(
        ledger.pending().await.unwrap()[0].context_operation,
        c.operation
    );
    fresh.cancel(&c).await.unwrap();
    stopped(&fresh, &c).await;
    drop(b);
    drop(fresh);
    let fresh = backend(root.path());
    assert_eq!(fresh.stop(&c).await.unwrap(), Progress::Confirmed);
    destroyed(&fresh, &c).await;
    destroyed(&fresh, &c).await;
    assert_eq!(fresh.acquire(&c, &r).await.unwrap(), e); // durable tombstone, never rerun
    assert_eq!(
        fresh.lookup(&c).await.unwrap().unwrap().state,
        ExecutionState::Destroyed
    );
}

#[tokio::test]
#[ignore = "requires privileged Linux test environment; spawns and terminates a separate supervisor process"]
async fn linux_backend_process_crash() {
    let c = context("process-crash");
    let r = request("sleep 30 & echo ready > child-started; wait");
    if let Ok(path) = std::env::var("RUNTRELL_CRASH_CHILD_DIR") {
        let b = backend(std::path::Path::new(&path));
        let e = b.acquire(&c, &r).await.unwrap();
        b.start(&c, &e).await.unwrap();
        child_started(std::path::Path::new(&path), &c).await;
        std::process::exit(23); // no destructors: OS closes the control lock
    }
    let root = root();
    let ledger = MemoryExecutionLedger::default();
    ledger
        .insert(ExecutionRecord::intent(&c, r.clone()))
        .await
        .unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "linux_backend_process_crash"])
        .env("RUNTRELL_CRASH_CHILD_DIR", root.path())
        // Keep nested libtest progress separate from the parent's named result.
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(23),
        "child stdout: {}\nchild stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let b = backend(root.path());
    let pending = ledger.pending().await.unwrap();
    assert_eq!(pending[0].context_operation, c.operation);
    assert_eq!(b.reconcile(&pending[0]).unwrap().execution.id, c.operation);
    let found = b.inventory().unwrap();
    assert!(
        found
            .iter()
            .any(|i| i.execution.as_ref().is_some_and(|e| e.id == c.operation))
    );
    for _ in 0..100 {
        if b.stop(&c).await.unwrap() == Progress::Confirmed {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(b.stop(&c).await.unwrap(), Progress::Confirmed);
    destroyed(&b, &c).await;
}

#[cfg(feature = "python")]
#[tokio::test]
#[ignore = "requires privileged Linux test environment with a host-prepared Python runtime"]
async fn linux_python_workspace_output_is_saved_before_cleanup() {
    use runtrell::{data::*, execution::python::*};
    let root = root();
    let b = backend(root.path());
    let c = context("python");
    let store = DataStore::memory(StoragePolicy::default());
    let access = Access::owner(Namespace {
        owner: c.claim.run.clone(),
        agent: "main".into(),
    });
    store
        .write(
            &access,
            "job.py",
            None,
            b"from pathlib import Path\nPath('answer.txt').write_text('42')\n",
        )
        .await
        .unwrap();
    let mut frozen = ExecutionRequest::process("/usr/bin/python3", vec![]);
    freeze_workspace(&store, &access, &mut frozen, 4096)
        .await
        .unwrap();
    let hash = runtrell::data::content_digest(&std::fs::read("/usr/bin/python3").unwrap());
    let version = std::env::var("RUNTRELL_PYTHON_VERSION").expect("exact host Python version");
    let py = PythonRuntime::new("/usr/bin/python3", &version, &hash).unwrap();
    let r = py
        .request(
            (frozen.input.unwrap(), frozen.input_files),
            "job.py",
            vec!["answer.txt".into()],
            request("").limits,
            DependencyPolicy::PreparedOnly,
        )
        .unwrap();
    let mut record = ExecutionRecord::intent(&c, r.clone());
    let e = b.acquire(&c, &r).await.unwrap();
    b.start(&c, &e).await.unwrap();
    let o = stopped(&b, &c).await;
    assert!(o.failure.is_none(), "{o:?}");
    record.observe(o);
    merge_process_copy(&store, &access, &record).await.unwrap();
    destroyed(&b, &c).await;
    let snapshot = store.snapshot(&access).await.unwrap();
    let f = snapshot
        .files
        .iter()
        .find(|f| f.path == "answer.txt")
        .unwrap();
    assert_eq!(
        store
            .read(&access, "answer.txt", f.revision, 0, 100)
            .await
            .unwrap()
            .bytes,
        b"42"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn linux_non_cgroup_directory_is_rejected_before_resource_acquisition() {
    let root = tempfile::tempdir().unwrap();
    let group = tempfile::tempdir().unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        LinuxBackend::new(
            root.path().canonicalize().unwrap(),
            group.path(),
            env!("CARGO_BIN_EXE_runtrell-exec-limit"),
            "/usr/bin/false",
            &["/bin/sh"],
            LinuxCpuBandwidth {
                quota_usec: 10000,
                period_usec: 10000
            }
        )
        .is_err()
    );
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    assert_eq!(std::fs::read_dir(group.path()).unwrap().count(), 0);
}

#[cfg(target_os = "linux")]
#[test]
fn linux_late_helper_cannot_join_after_stop_marker() {
    let control = tempfile::tempdir().unwrap();
    let group = tempfile::tempdir().unwrap();
    std::fs::write(control.path().join("cancel"), b"").unwrap();
    std::fs::write(group.path().join("cgroup.procs"), b"untouched").unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_runtrell-exec-limit"))
        .arg(u64::MAX.to_string())
        .arg(u64::MAX.to_string())
        .arg("--cgroup")
        .arg(std::process::id().to_string())
        .arg(group.path())
        .arg(control.path())
        .arg("/usr/bin/false")
        .env_clear()
        // Expected CANCELLED_BEFORE_JOIN stderr is not a libtest result.
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "late helper unexpectedly succeeded; child stdout: {}\nchild stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("CANCELLED_BEFORE_JOIN"),
        "child stdout: {}\nchild stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read(group.path().join("cgroup.procs")).unwrap(),
        b"untouched"
    );
}

#[tokio::test]
#[ignore = "requires privileged Linux test environment; verifies payload privilege separation"]
async fn linux_payload_has_no_privileges() {
    let root = root();
    let b = backend(root.path());
    let c = context("privileges");
    let r = request(
        r#"/usr/bin/python3 -c 'import json,os; print(json.dumps({"uid":os.getuid(),"gid":os.getgid(),"userns":os.readlink("/proc/self/ns/user"),"caps":{k:v.strip() for k,v in (line.split(":",1) for line in open("/proc/self/status") if line.startswith(("Cap","NoNewPrivs")))}}))'"#,
    );
    let e = b.acquire(&c, &r).await.unwrap();
    b.start(&c, &e).await.unwrap();
    let o = stopped(&b, &c).await;
    assert!(o.failure.is_none(), "payload must execute: {o:?}");
    let stdout: Vec<u8> = serde_json::from_value(o.result.unwrap()["stdout"].clone()).unwrap();
    let result: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(result["uid"], 65534);
    assert_eq!(result["gid"], 65534);
    assert_ne!(
        result["userns"].as_str().unwrap(),
        std::fs::read_link("/proc/self/ns/user")
            .unwrap()
            .to_str()
            .unwrap()
    );
    assert_eq!(result["caps"]["NoNewPrivs"], "1");
    for name in ["CapInh", "CapPrm", "CapEff", "CapBnd", "CapAmb"] {
        assert_eq!(
            u64::from_str_radix(result["caps"][name].as_str().unwrap(), 16).unwrap(),
            0,
            "{name}"
        );
    }
    destroyed(&b, &c).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires delegated cgroup v2, host mounts and bubblewrap user namespaces"]
async fn linux_concurrent_runs_reclaim_independently() {
    let root = root();
    let b = backend(root.path());
    let mut jobs = tokio::task::JoinSet::new();
    for index in 0..8 {
        let b = b.clone();
        jobs.spawn(async move {
            let mut c = context(&format!("parallel-{index}"));
            c.claim.run.id = format!("run-{index}");
            let r = request("printf data > scratch-data; sleep 0.2");
            let e = b.acquire(&c, &r).await.unwrap();
            b.start(&c, &e).await.unwrap();
            stopped(&b, &c).await;
            destroyed(&b, &c).await;
        });
    }
    while let Some(done) = jobs.join_next().await {
        done.unwrap();
    }
    for item in b.inventory().unwrap() {
        assert_eq!(item.state, ExecutionState::Destroyed);
    }
}

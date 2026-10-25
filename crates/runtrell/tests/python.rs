// SPDX-License-Identifier: Apache-2.0
#![cfg(feature = "python")]
use runtrell::execution::python::*;
#[test]
fn runtime_requires_pinned_absolute_identity_and_fetch_is_fail_closed() {
    assert!(PythonRuntime::new("python3", "3.12", "hash").is_err());
    assert!(PythonRuntime::new("/usr/bin/python3", "", "hash").is_err());
    assert!(
        DependencyPolicy::Fetch {
            network_allow: vec!["example.org".into()],
            credential_scopes: vec![],
            budget_micro_usd: 1,
            deadline_ms: 1
        }
        .validate()
        .is_err()
    );
    assert!(DependencyPolicy::PreparedOnly.validate().is_ok());
}

#[tokio::test]
async fn frozen_request_is_bound_to_runtime_snapshot_and_offline_policy() {
    use runtrell::{data::*, execution::*, *};
    let interpreter = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(interpreter.path(), b"synthetic-host-runtime").unwrap();
    let hash = content_digest(b"synthetic-host-runtime");
    let runtime =
        PythonRuntime::new(interpreter.path().to_str().unwrap(), "3.12.3", &hash).unwrap();
    let store = DataStore::memory(StoragePolicy::default());
    let access = Access::owner(Namespace {
        owner: RunHandle {
            tenant: "t".into(),
            id: "r".into(),
        },
        agent: "main".into(),
    });
    store
        .write(&access, "job.py", None, b"print(42)")
        .await
        .unwrap();
    let mut frozen = ExecutionRequest::process("unused", vec![]);
    freeze_workspace(&store, &access, &mut frozen, 4096)
        .await
        .unwrap();
    let input = (frozen.input.unwrap(), frozen.input_files);
    let request = runtime
        .request(
            input.clone(),
            "job.py",
            vec!["out".into()],
            ExecutionLimits::default(),
            DependencyPolicy::PreparedOnly,
        )
        .unwrap();
    assert!(request.limits.network_allow.is_empty());
    assert!(request.credential_scopes.is_empty());
    assert_eq!(request.input_files["job.py"], b"print(42)");
    let Program::Process { args, .. } = request.program;
    assert_eq!(&args[..2], ["-I", "-B"]);
    std::fs::write(interpreter.path(), b"changed").unwrap();
    assert!(
        runtime
            .request(
                input,
                "job.py",
                vec![],
                ExecutionLimits::default(),
                DependencyPolicy::PreparedOnly
            )
            .is_err()
    );
}

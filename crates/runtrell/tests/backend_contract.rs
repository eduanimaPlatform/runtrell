// SPDX-License-Identifier: Apache-2.0
use runtrell::{data::*, *};
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, Ordering},
    },
};

#[derive(Default)]
struct FakeExternal {
    objects: Mutex<BTreeMap<String, (u64, Vec<u8>)>>,
    partial_uploads: Mutex<BTreeMap<String, Vec<u8>>>,
    // Simulate a transport failure before commit or after durable commit.
    failure: AtomicU8,
}
impl NamespaceBackend for FakeExternal {
    fn load(&self, key: &str) -> Result<(String, Option<Vec<u8>>)> {
        let objects = self.objects.lock().unwrap();
        Ok(objects
            .get(key)
            .map(|(v, b)| (v.to_string(), Some(b.clone())))
            .unwrap_or(("0".into(), None)))
    }
    fn compare_exchange(&self, key: &str, expected: &str, payload: &[u8]) -> Result<()> {
        let failure = self.failure.swap(0, Ordering::SeqCst);
        if failure == 1 {
            return Err(Error::Storage);
        }
        let mut objects = self.objects.lock().unwrap();
        let version = objects.get(key).map_or(0, |(v, _)| *v);
        if version.to_string() != expected {
            return Err(Error::Conflict);
        }
        // Uploading partial bytes is not publication: the committed map is the marker.
        let mut partials = self.partial_uploads.lock().unwrap();
        // The namespace CAS lock owns transport staging. An ended request's
        // incomplete upload is either resumed byte-for-byte or replaced by the
        // next owner, never exposed as a committed namespace.
        let mut upload = partials.remove(key).unwrap_or_default();
        if !payload.starts_with(&upload) {
            upload.clear();
        }
        if upload.is_empty() {
            upload.extend_from_slice(&payload[..payload.len() / 2]);
        }
        if failure == 3 {
            partials.insert(key.into(), upload);
            return Err(Error::Storage);
        }
        upload.extend_from_slice(&payload[upload.len()..]);
        objects.insert(key.into(), (version + 1, upload));
        if failure == 2 {
            return Err(Error::Storage);
        }
        Ok(())
    }
}
fn access() -> Access {
    Access::owner(Namespace {
        owner: RunHandle {
            tenant: "tenant".into(),
            id: "run".into(),
        },
        agent: "agent".into(),
    })
}
async fn contract(open: impl Fn() -> DataStore) {
    let control = memory::MemoryStore::default();
    let account = control
        .create_account("tenant", Amount::new(100, 100), None)
        .await
        .unwrap();
    let mut spec = RunSpec::new("tenant", "run", account, "answer");
    spec.basis_revision = 1;
    spec.config_version = "config".into();
    spec.validator = "schema".into();
    let handle = control.start(spec).await.unwrap();
    let claim = control.claim(&handle, "worker", 0, 100).await.unwrap();
    let run = control.get(&handle).await.unwrap();
    let a = Access::owner(Namespace {
        owner: handle,
        agent: "agent".into(),
    });
    let s = open();
    let version = s.write(&a, "answer.json", None, b"{}").await.unwrap();
    let tool = file_tools::FileTool {
        store: s.clone(),
        access: a.clone(),
        action: file_tools::FileAction::Read,
    };
    let descriptor = tool.descriptor();
    let context = ToolContext {
        claim,
        principal: run.spec.principal.clone(),
        operation: "read".into(),
        attempt: 1,
        deadline: None,
        permission: descriptor.permission,
        account_binding: descriptor.account,
    };
    assert!(matches!(
        tool.invoke(
            &context,
            json!({"path":"answer.json","revision":version.revision,"offset":0,"length":2})
        )
        .await
        .unwrap(),
        ExternalResult::Applied { .. }
    ));
    let wrapped = file_tools::StoredOutputTool {
        inner: tool,
        store: s.clone(),
        access: a.clone(),
        inline_limit: 4,
        now: 0,
    };
    let ExternalResult::Applied { output, .. } = wrapped
        .invoke(
            &context,
            json!({"path":"answer.json","revision":version.revision,"offset":0,"length":2}),
        )
        .await
        .unwrap()
    else {
        panic!("stored tool output")
    };
    assert!(matches!(
        serde_json::from_value::<StoredOutput>(output).unwrap(),
        StoredOutput::External { .. }
    ));
    let before = s.context(&a).await.unwrap();
    s.append_context(
        &a,
        ContextItem {
            source: Source::Input,
            value: json!("fact"),
        },
    )
    .await
    .unwrap();
    assert!(
        open()
            .commit_compaction(&a, &before, "stale", true)
            .await
            .is_err()
    );
    assert_eq!(open().context(&a).await.unwrap().history.len(), 1);
    let base = s.snapshot(&a).await.unwrap();
    let staged = s.stage(&a, b"payload", DataClass::Intermediate, 0).unwrap();
    assert!(matches!(
        s.artifact(&a, &staged.id).await,
        Err(Error::NotFound)
    ));
    // Stop after staging; only persisted state is retained by the new client.
    let id = staged.id.clone();
    drop(s);
    let s = open();
    let staged = s.staged(&a, &id).unwrap();
    let blob = s.save(&a, staged.clone(), 0).await.unwrap();
    assert_eq!(s.save(&a, staged, 0).await.unwrap(), blob);
    drop(s);
    let s = open();
    assert_eq!(
        s.artifact_status(&a, &blob).await.unwrap(),
        ArtifactStatus::Saved
    );
    let validator = schema::SchemaValidator {
        key: ValidatorKey::new("schema", "1"),
        schema: json!({"type":"object"}),
    };
    let target = Target::artifact(base.clone(), json!({}), 1, "config").with_artifact(blob.clone());
    let adapter = DataValidator {
        store: s.clone(),
        access: a.clone(),
        validator: schema::SchemaValidator {
            key: validator.key(),
            schema: json!({"type":"object"}),
        },
        workspace: true,
        artifacts: vec![blob.clone()],
    };
    let adapter_receipt = adapter.validate(&run, &json!({})).unwrap();
    let evidence = s.validate(&a, &target, &validator).await.unwrap();
    drop(s);
    let s = open();
    let mut forged = evidence.clone();
    forged.receipt.hash = "forged".into();
    assert_eq!(
        s.finalize_target(&a, &target, &[forged], &[validator.key()])
            .await,
        Err(Error::Conflict)
    );
    let finalizer = DataFinalizer {
        store: s.clone(),
        access: a.clone(),
        required: vec![validator.key()],
        workspace: true,
        artifacts: vec![blob.clone()],
    };
    let failed = s
        .validate(
            &a,
            &target,
            &schema::SchemaValidator {
                key: validator.key(),
                schema: json!(false),
            },
        )
        .await
        .unwrap();
    assert!(!failed.receipt.passed);
    assert!(finalizer.finalize(&run, json!({}), failed.receipt).is_err());
    assert_eq!(
        s.artifact_status(&a, &blob).await.unwrap(),
        ArtifactStatus::Saved
    );
    let adapter_completion = finalizer
        .finalize(&run, json!({}), adapter_receipt)
        .unwrap();
    let completion = s
        .finalize_target(
            &a,
            &target,
            std::slice::from_ref(&evidence),
            &[validator.key()],
        )
        .await
        .unwrap();
    assert_eq!(adapter_completion.id, completion);
    drop(s);
    let s = open();
    assert_eq!(
        s.artifact_status(&a, &blob).await.unwrap(),
        ArtifactStatus::Finalized {
            completion_id: completion.clone()
        }
    );
    assert_eq!(
        s.finalize_target(
            &a,
            &target,
            std::slice::from_ref(&evidence),
            &[validator.key()]
        )
        .await
        .unwrap(),
        completion
    );
    s.write(&a, "answer.json", Some(version.revision), b"changed")
        .await
        .unwrap();
    assert_eq!(
        s.finalize_target(&a, &target, &[evidence], &[validator.key()])
            .await,
        Err(Error::Conflict)
    );
    assert!(s.merge(&a, &base, vec![]).await.is_err());
    let orphan = s.stage(&a, b"orphan", DataClass::Intermediate, 0).unwrap();
    assert_eq!(s.cleanup_staging(&a, 0).unwrap(), 0);
    let other = Access::for_actor(
        Namespace {
            owner: RunHandle {
                tenant: "tenant".into(),
                id: "intruder".into(),
            },
            agent: "agent".into(),
        },
        a.namespace().clone(),
    );
    assert_eq!(
        s.cleanup_staging(&other, u64::MAX),
        Err(Error::PermissionDenied)
    );
    assert_eq!(s.cleanup_staging(&a, u64::MAX).unwrap(), 1);
    assert!(matches!(s.staged(&a, &orphan.id), Err(Error::NotFound)));
    assert!(matches!(
        s.artifact_status(&a, &blob).await.unwrap(),
        ArtifactStatus::Finalized { .. }
    ));
    let revision = s.snapshot(&a).await.unwrap().revision;
    let until = s.retain(&a, DataClass::Log, 0).await.unwrap();
    let live = s
        .stage(&a, b"unpublished log", DataClass::Log, until)
        .unwrap();
    assert_eq!(
        s.cleanup_class(&a, DataClass::Log, revision, until)
            .await
            .unwrap(),
        Progress::Pending
    );
    assert_eq!(
        s.cleanup_class(&a, DataClass::Log, revision, u64::MAX)
            .await
            .unwrap(),
        Progress::Confirmed
    );
    assert!(matches!(s.staged(&a, &live.id), Err(Error::NotFound)));
}
#[tokio::test]
async fn memory_local_external_share_contract_and_restart_boundaries() {
    let memory = DataStore::memory(StoragePolicy::default());
    contract(|| memory.clone()).await;
    let root = tempfile::tempdir().unwrap();
    contract(|| {
        DataStore::local(
            &root.path().canonicalize().unwrap(),
            StoragePolicy::default(),
        )
        .unwrap()
    })
    .await;
    let backend = Arc::new(FakeExternal::default());
    contract(|| DataStore::external(backend.clone(), StoragePolicy::default())).await;
}
#[tokio::test]
async fn external_partial_upload_and_response_loss_at_each_publication_boundary() {
    for failure in [1, 2, 3] {
        for boundary in 0..4 {
            let backend = Arc::new(FakeExternal::default());
            let open = || DataStore::external(backend.clone(), StoragePolicy::default());
            let s = open();
            let a = access();
            let validator = schema::SchemaValidator {
                key: ValidatorKey::new("schema", "1"),
                schema: json!(true),
            };
            if boundary == 0 {
                backend.failure.store(failure, Ordering::SeqCst);
                assert!(s.stage(&a, b"blob", DataClass::Intermediate, 0).is_err());
                // A lost staging response is garbage collectable even without its ID.
                assert_eq!(
                    backend.partial_uploads.lock().unwrap().len(),
                    usize::from(failure == 3)
                );
                let gc = open().cleanup_staging(&a, u64::MAX).unwrap();
                assert!(backend.partial_uploads.lock().unwrap().is_empty());
                assert_eq!(gc, usize::from(failure == 2));
                continue;
            }
            let staged = s.stage(&a, b"blob", DataClass::Intermediate, 0).unwrap();
            if boundary == 1 {
                backend.failure.store(failure, Ordering::SeqCst);
                assert!(s.save(&a, staged.clone(), 0).await.is_err());
            }
            let blob = open().save(&a, staged, 0).await.unwrap();
            let target = Target::structured(json!({}), 1, "config").with_artifact(blob.clone());
            if boundary == 2 {
                backend.failure.store(failure, Ordering::SeqCst);
                assert!(s.validate(&a, &target, &validator).await.is_err());
            }
            let evidence = open().validate(&a, &target, &validator).await.unwrap();
            if boundary == 3 {
                backend.failure.store(failure, Ordering::SeqCst);
                assert!(
                    s.finalize_target(
                        &a,
                        &target,
                        std::slice::from_ref(&evidence),
                        &[validator.key()]
                    )
                    .await
                    .is_err()
                );
            }
            let id = open()
                .finalize_target(&a, &target, &[evidence], &[validator.key()])
                .await
                .unwrap();
            assert_eq!(
                open().artifact_status(&a, &blob).await.unwrap(),
                ArtifactStatus::Finalized { completion_id: id }
            );
        }
    }
}
#[tokio::test]
async fn local_collects_only_orphan_transaction_uploads_under_namespace_lock() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().canonicalize().unwrap();
    let s = DataStore::local(&path, StoragePolicy::default()).unwrap();
    let a = access();
    s.write(&a, "saved", None, b"committed").await.unwrap();
    let directory = std::fs::read_dir(&path)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let orphan = directory.join(format!("stage-{}", uuid::Uuid::new_v4()));
    let unrelated = directory.join("stage-unrelated");
    std::fs::write(&orphan, b"partial state").unwrap();
    std::fs::write(&unrelated, b"preserve").unwrap();
    drop(s);
    let s = DataStore::local(&path, StoragePolicy::default()).unwrap();
    assert_eq!(s.snapshot(&a).await.unwrap().files.len(), 1);
    assert!(!orphan.exists());
    assert_eq!(std::fs::read(unrelated).unwrap(), b"preserve");
}

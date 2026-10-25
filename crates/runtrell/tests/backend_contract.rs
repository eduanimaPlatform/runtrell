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
        recovery_proof: None,
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
    let validator = Arc::new(schema::SchemaValidator {
        key: ValidatorKey::new("schema", "1"),
        schema: json!({"type":"object"}),
    });
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
    let evidence = s.validate(&a, &target, validator.clone()).await.unwrap();
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
            Arc::new(schema::SchemaValidator {
                key: validator.key(),
                schema: json!(false),
            }),
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
            let validator = Arc::new(schema::SchemaValidator {
                key: ValidatorKey::new("schema", "1"),
                schema: json!(true),
            });
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
                assert!(s.validate(&a, &target, validator.clone()).await.is_err());
            }
            let evidence = open()
                .validate(&a, &target, validator.clone())
                .await
                .unwrap();
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

// Only the public ArtifactStore contract is used here, including recovery.
async fn stable_raw_contract<S: ArtifactStore>(open: impl Fn() -> S) {
    let a = access();
    let spec = ArtifactSpec {
        id: "job-result-sha256".into(),
        media_type: "image/png".into(),
        class: DataClass::Intermediate,
    };
    let bytes: Vec<u8> = (0..600_000).map(|i| (i % 256) as u8).collect();
    let s = open();
    let staged = s.stage_named(&a, spec.clone(), &bytes, 10).await.unwrap();
    assert_eq!(s.staged(&a, &spec.id).await.unwrap(), staged);
    assert!(matches!(
        s.stage_named(&a, spec.clone(), b"different", 11).await,
        Err(Error::Conflict)
    ));
    drop(s);
    // No in-process staging handle is required after reopening.
    let s = open();
    let resumed = s.stage_named(&a, spec.clone(), &bytes, 100).await.unwrap();
    assert_eq!(resumed, staged);
    let saved = s
        .save(&a, s.staged(&a, &spec.id).await.unwrap(), 100)
        .await
        .unwrap();
    assert_eq!(saved.hash, content_digest(&bytes));
    assert_eq!(saved.media_type, "image/png");
    drop(s);
    let s = open();
    let again = s.stage_named(&a, spec.clone(), &bytes, 1000).await.unwrap();
    assert_eq!(s.save(&a, again, 1000).await.unwrap(), saved);
    assert_eq!(s.save(&a, staged, 2000).await.unwrap(), saved);
    assert!(matches!(s.staged(&a, &spec.id).await, Err(Error::NotFound)));
    assert!(matches!(
        s.stage_named(&a, spec.clone(), b"changed", 1000).await,
        Err(Error::Conflict)
    ));
    let mut changed = spec.clone();
    changed.media_type = "application/octet-stream".into();
    assert!(matches!(
        s.stage_named(&a, changed, &bytes, 1000).await,
        Err(Error::Conflict)
    ));
    let mut changed = spec.clone();
    changed.class = DataClass::Final;
    assert!(matches!(
        s.stage_named(&a, changed, &bytes, 1000).await,
        Err(Error::Conflict)
    ));
    let slice = s.read_artifact(&a, &saved, 255_999, 4096).await.unwrap();
    assert_eq!(slice.bytes, bytes[255_999..260_095]);
    assert_eq!(slice.total, bytes.len() as u64);
    assert!(slice.truncated);
    let end = s.read_artifact(&a, &saved, 599_990, 100).await.unwrap();
    assert_eq!(end.bytes, bytes[599_990..]);
    assert!(!end.truncated);
    for namespace in [
        Namespace {
            owner: RunHandle {
                tenant: "tenant".into(),
                id: "other-run".into(),
            },
            agent: "agent".into(),
        },
        Namespace {
            owner: RunHandle {
                tenant: "other-tenant".into(),
                id: "run".into(),
            },
            agent: "agent".into(),
        },
        Namespace {
            owner: a.namespace().owner.clone(),
            agent: "other-agent".into(),
        },
    ] {
        let intruder = Access::for_actor(namespace.clone(), a.namespace().clone());
        assert!(matches!(
            s.stage_named(&intruder, spec.clone(), &bytes, 0).await,
            Err(Error::PermissionDenied)
        ));
        assert!(matches!(
            s.staged(&intruder, &spec.id).await,
            Err(Error::PermissionDenied)
        ));
        assert_eq!(
            s.save(
                &intruder,
                s.stage_named(&a, spec.clone(), &bytes, 0).await.unwrap(),
                0
            )
            .await,
            Err(Error::PermissionDenied)
        );
        assert_eq!(
            s.read_artifact(&intruder, &saved, 0, 1).await,
            Err(Error::PermissionDenied)
        );
        let own = Access::owner(namespace);
        assert_eq!(
            s.read_artifact(&own, &saved, 0, 1).await,
            Err(Error::PermissionDenied)
        );
        let own_staged = s
            .stage_named(&own, spec.clone(), b"independent", 0)
            .await
            .unwrap();
        let own_saved = s.save(&own, own_staged, 0).await.unwrap();
        assert_ne!(own_saved.owner, saved.owner);
    }
    // A no-op retry must not advance the artifact revision.
    let next = s
        .put(&a, b"next", DataClass::Intermediate, 2000)
        .await
        .unwrap();
    assert_eq!(next.revision, saved.revision + 1);
}
#[tokio::test]
async fn stable_raw_bytes_memory_local_external_contract() {
    let memory = DataStore::memory(StoragePolicy::default());
    stable_raw_contract(|| memory.clone()).await;
    let root = tempfile::tempdir().unwrap();
    stable_raw_contract(|| {
        DataStore::local(
            &root.path().canonicalize().unwrap(),
            StoragePolicy::default(),
        )
        .unwrap()
    })
    .await;
    let backend = Arc::new(FakeExternal::default());
    stable_raw_contract(|| DataStore::external(backend.clone(), StoragePolicy::default())).await;
}

#[tokio::test]
async fn stable_raw_bytes_recovers_lost_stage_and_save_responses_without_republication() {
    for failure in [1, 2, 3] {
        for boundary in 0..2 {
            let backend = Arc::new(FakeExternal::default());
            let open = || DataStore::external(backend.clone(), StoragePolicy::default());
            let a = access();
            let spec = ArtifactSpec {
                id: "stable-job".into(),
                media_type: "application/custom".into(),
                class: DataClass::Intermediate,
            };
            let s = open();
            if boundary == 0 {
                backend.failure.store(failure, Ordering::SeqCst);
                assert!(
                    ArtifactStore::stage_named(&s, &a, spec.clone(), b"\0\xffraw", 0)
                        .await
                        .is_err()
                );
            }
            drop(s);
            let s = open();
            let staged = ArtifactStore::stage_named(&s, &a, spec.clone(), b"\0\xffraw", 0)
                .await
                .unwrap();
            if boundary == 1 {
                backend.failure.store(failure, Ordering::SeqCst);
                assert!(s.save(&a, staged.clone(), 0).await.is_err());
            }
            drop(s);
            let s = open();
            let saved = s.save(&a, staged, 0).await.unwrap();
            let before = backend.objects.lock().unwrap().clone();
            // A completed result must be returned without another backend commit.
            backend.failure.store(1, Ordering::SeqCst);
            let staged = ArtifactStore::stage_named(&s, &a, spec, b"\0\xffraw", 99)
                .await
                .unwrap();
            assert_eq!(s.save(&a, staged, 99).await.unwrap(), saved);
            assert_eq!(*backend.objects.lock().unwrap(), before);
            assert_eq!(backend.failure.load(Ordering::SeqCst), 1);
            assert!(backend.partial_uploads.lock().unwrap().is_empty());
        }
    }
}

#[tokio::test]
async fn stable_raw_bytes_checks_tokens_and_metadata_before_publication() {
    let mut policy = StoragePolicy::default();
    policy
        .classes
        .get_mut(&DataClass::Intermediate)
        .unwrap()
        .max_bytes = 64;
    let s = DataStore::memory(policy);
    let a = access();
    let spec = ArtifactSpec {
        id: "stable".into(),
        media_type: "application/octet-stream".into(),
        class: DataClass::Intermediate,
    };
    let staged = ArtifactStore::stage_named(&s, &a, spec.clone(), b"content", 0)
        .await
        .unwrap();
    assert!(matches!(
        ArtifactStore::stage_named(&s, &a, spec.clone(), &[0; 65], 0).await,
        Err(Error::Conflict)
    ));
    for after_save in [false, true] {
        if after_save {
            s.save(&a, staged.clone(), 0).await.unwrap();
        }
        let mut forged = staged.clone();
        forged.bytes = b"forged".to_vec();
        assert_eq!(s.save(&a, forged, 0).await, Err(Error::Conflict));
        let mut forged = staged.clone();
        forged.media_type = "image/png".into();
        assert_eq!(s.save(&a, forged, 0).await, Err(Error::Conflict));
        let mut forged = staged.clone();
        forged.owner.agent = "forged".into();
        assert_eq!(s.save(&a, forged, 0).await, Err(Error::PermissionDenied));
    }
    for (id, media_type) in [
        ("", "image/png"),
        ("bad\nid", "image/png"),
        ("valid", ""),
        ("valid", "image/png\r\n"),
    ] {
        let invalid = ArtifactSpec {
            id: id.into(),
            media_type: media_type.into(),
            class: spec.class,
        };
        assert!(matches!(
            ArtifactStore::stage_named(&s, &a, invalid, b"content", 0).await,
            Err(Error::Invalid(_))
        ));
    }
}

#[tokio::test]
async fn local_stable_raw_bytes_survive_process_exit_at_stage_and_save() {
    let spec = ArtifactSpec {
        id: "external-job-result".into(),
        media_type: "application/custom".into(),
        class: DataClass::Intermediate,
    };
    let a = access();
    if let Ok(phase) = std::env::var("RUNTRELL_ARTIFACT_STOP_PHASE") {
        let root = std::env::var("RUNTRELL_ARTIFACT_STOP_ROOT").unwrap();
        let s = DataStore::local(std::path::Path::new(&root), StoragePolicy::default()).unwrap();
        let staged = ArtifactStore::stage_named(&s, &a, spec, b"\0\xffresult", 0)
            .await
            .unwrap();
        if phase == "save" {
            s.save(&a, staged, 0).await.unwrap();
        } else {
            assert_eq!(phase, "stage");
        }
        // Stop the worker without unwinding or running store/runtime destructors.
        std::process::exit(0);
    }
    for phase in ["stage", "save"] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().canonicalize().unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "local_stable_raw_bytes_survive_process_exit_at_stage_and_save",
                "--nocapture",
            ])
            .env("RUNTRELL_ARTIFACT_STOP_PHASE", phase)
            .env("RUNTRELL_ARTIFACT_STOP_ROOT", &path)
            .status()
            .unwrap();
        assert!(status.success());
        let s = DataStore::local(&path, StoragePolicy::default()).unwrap();
        let previous = if phase == "save" {
            Some(s.artifact(&a, &spec.id).await.unwrap())
        } else {
            assert!(matches!(
                s.artifact(&a, &spec.id).await,
                Err(Error::NotFound)
            ));
            assert_eq!(s.staged(&a, &spec.id).unwrap().bytes, b"\0\xffresult");
            None
        };
        let staged = ArtifactStore::stage_named(&s, &a, spec.clone(), b"\0\xffresult", 10)
            .await
            .unwrap();
        let saved = s.save(&a, staged, 10).await.unwrap();
        assert_eq!(saved.id, spec.id);
        assert_eq!(saved.revision, 1);
        assert_eq!(saved.media_type, spec.media_type);
        if let Some(previous) = previous {
            assert_eq!(saved, previous);
        }
        assert_eq!(
            s.read_artifact(&a, &saved, 0, 100).await.unwrap().bytes,
            b"\0\xffresult"
        );
    }
}

#[tokio::test]
async fn random_artifact_stage_preserves_explicit_write_sharing_contract() {
    let s = DataStore::memory(StoragePolicy::default());
    let a = access();
    let shared = s.share_agent(&a, "child", true, 0).await.unwrap();
    let staged = s
        .stage(&shared, b"shared bytes", DataClass::Intermediate, 0)
        .unwrap();
    let spec = ArtifactSpec {
        id: "owner-stable-id".into(),
        media_type: "application/octet-stream".into(),
        class: DataClass::Intermediate,
    };
    assert!(matches!(
        ArtifactStore::stage_named(&s, &shared, spec, b"owner only", 0).await,
        Err(Error::PermissionDenied)
    ));
    let saved = s.save(&shared, staged, 0).await.unwrap();
    assert_eq!(saved.owner, *a.namespace());
    assert_eq!(
        s.read_artifact(&a, &saved, 0, 100).await.unwrap().bytes,
        b"shared bytes"
    );
}

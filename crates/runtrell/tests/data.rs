// SPDX-License-Identifier: Apache-2.0
use runtrell::{data::*, *};
use serde_json::json;
fn ns(run: &str) -> Namespace {
    Namespace {
        owner: RunHandle {
            tenant: "t".into(),
            id: run.into(),
        },
        agent: "main".into(),
    }
}
fn drive(f: impl std::future::Future<Output = ()>) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let _enter = rt.enter();
    let mut f = std::pin::pin!(f);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    loop {
        if f.as_mut().poll(&mut cx).is_ready() {
            break;
        }
    }
}
#[test]
fn atomic_exact_replace_and_stale_copy() {
    drive(async {
        let s = DataStore::memory(StoragePolicy::default());
        let a = Access::owner(ns("r"));
        let v = s
            .write(&a, "a", None, b"first\r\nsecond\r\n")
            .await
            .unwrap();
        let base = s.snapshot(&a).await.unwrap();
        let edit = ExactEdit {
            path: "a".into(),
            expected_digest: v.hash.clone(),
            old_text: "first".into(),
            new_text: "next".into(),
        };
        let done = s
            .exact_replace(&a, base.revision, vec![edit.clone()])
            .await
            .unwrap();
        assert_eq!(
            s.read(&a, "a", done.revision, 0, 100).await.unwrap().bytes,
            b"next\r\nsecond\r\n"
        );
        assert!(
            s.exact_replace(&a, done.revision, vec![edit])
                .await
                .is_err()
        );
        assert!(
            s.merge(
                &a,
                &base,
                vec![Change::Write {
                    path: "a".into(),
                    expected: Some(v.revision),
                    bytes: b"stale".to_vec()
                }]
            )
            .await
            .is_err()
        );
        let before = s.snapshot(&a).await.unwrap();
        assert!(
            s.commit(
                &a,
                before.revision,
                vec![
                    Change::Write {
                        path: "b".into(),
                        expected: None,
                        bytes: b"ok".to_vec()
                    },
                    Change::Delete {
                        path: "missing".into(),
                        expected: 1
                    }
                ]
            )
            .await
            .is_err()
        );
        assert_eq!(s.snapshot(&a).await.unwrap(), before);
    });
}
#[test]
fn isolation_paths_ranges_and_staging() {
    drive(async {
        let s = DataStore::memory(StoragePolicy::default());
        let a = Access::owner(ns("r"));
        for path in [
            "../x", "/tmp/x", "a//b", "a/./b", "a/../b", "a:stream", "a\\b", "",
        ] {
            assert!(s.write(&a, path, None, b"bad").await.is_err(), "{path}");
        }
        let pending = s
            .stage(&a, b"large output", DataClass::Intermediate, 0)
            .unwrap();
        assert!(s.artifact(&a, &pending.id).await.is_err());
        let artifact = s.save(&a, pending, 0).await.unwrap();
        let page = s.read_artifact(&a, &artifact, 6, 3).await.unwrap();
        assert_eq!(page.bytes, b"out");
        assert!(page.truncated);
        assert!(
            s.read_artifact(&Access::owner(ns("other")), &artifact, 0, 10)
                .await
                .is_err()
        );
        assert!(
            s.snapshot(&Access::for_actor(ns("other"), ns("r")))
                .await
                .is_err()
        );
    });
}
#[test]
fn validation_rejects_old_evidence_and_forged_receipt() {
    drive(async {
        let s = DataStore::memory(StoragePolicy::default());
        let a = Access::owner(ns("r"));
        s.write(&a, "a", None, b"ok").await.unwrap();
        let snap = s.snapshot(&a).await.unwrap();
        let target = Target::artifact(snap.clone(), json!({"decision":"keep"}), 1, "cfg");
        let validator = std::sync::Arc::new(Rules);
        let receipt = s.validate(&a, &target, validator.clone()).await.unwrap();
        let mut forged = receipt.clone();
        forged.receipt.hash = "fake".into();
        assert!(
            s.finalize_target(&a, &target, &[forged], &[ValidatorKey::new("rules", "1")])
                .await
                .is_err()
        );
        let v = snap.files[0].clone();
        s.write(&a, "a", Some(v.revision), b"changed")
            .await
            .unwrap();
        assert!(
            s.finalize_target(&a, &target, &[receipt], &[ValidatorKey::new("rules", "1")])
                .await
                .is_err()
        );
        let target = Target::structured(json!({"decision":"keep"}), 2, "cfg");
        let receipt = s.validate(&a, &target, validator.clone()).await.unwrap();
        let done = s
            .finalize_target(
                &a,
                &target,
                std::slice::from_ref(&receipt),
                &[ValidatorKey::new("rules", "1")],
            )
            .await
            .unwrap();
        assert_eq!(
            done,
            s.finalize_target(&a, &target, &[receipt], &[ValidatorKey::new("rules", "1")])
                .await
                .unwrap()
        );
    });
}
struct Rules;
impl TargetValidator for Rules {
    fn key(&self) -> ValidatorKey {
        ValidatorKey::new("rules", "1")
    }
    fn check(&self, _: &Target) -> Result<Vec<Diagnostic>> {
        Ok(vec![])
    }
}
#[test]
fn compaction_cas_keeps_new_history_and_provenance() {
    drive(async {
        let s = DataStore::memory(StoragePolicy::default());
        let a = Access::owner(ns("r"));
        s.append_context(
            &a,
            ContextItem {
                source: Source::Input,
                value: json!("input"),
            },
        )
        .await
        .unwrap();
        let base = s.context(&a).await.unwrap();
        s.append_context(
            &a,
            ContextItem {
                source: Source::Tool,
                value: json!("new"),
            },
        )
        .await
        .unwrap();
        assert!(
            s.commit_compaction(&a, &base, "summary", true)
                .await
                .is_err()
        );
        assert_eq!(s.context(&a).await.unwrap().history.len(), 2);
        let base = s.context(&a).await.unwrap();
        assert!(
            s.commit_compaction(&a, &base, "partial", false)
                .await
                .is_err()
        );
        s.commit_compaction(&a, &base, "summary", true)
            .await
            .unwrap();
        let done = s.context(&a).await.unwrap();
        assert_eq!(done.facts, base.facts);
        assert_eq!(done.high_water, 2);
    });
}
#[test]
fn exact_replace_all_refusals_and_atomic_batch() {
    drive(async {
        let s = DataStore::memory(StoragePolicy::default());
        let a = Access::owner(ns("r"));
        for (path, bytes, old, code) in [
            ("zero", b"abc".as_slice(), "x", "NO_MATCH"),
            ("many", b"aaa".as_slice(), "aa", "MULTIPLE_MATCHES"),
            ("binary", b"a\0b".as_slice(), "a", "BINARY"),
            ("control", b"a\x01b".as_slice(), "a", "BINARY"),
            ("utf8", b"\xff".as_slice(), "a", "NON_UTF8"),
            ("empty", b"abc".as_slice(), "", "EMPTY_MATCH"),
        ] {
            let v = s.write(&a, path, None, bytes).await.unwrap();
            let before = s.snapshot(&a).await.unwrap();
            let e = ExactEdit {
                path: path.into(),
                expected_digest: v.hash,
                old_text: old.into(),
                new_text: "new".into(),
            };
            let err = s
                .exact_replace(&a, before.revision, vec![e])
                .await
                .unwrap_err();
            assert!(err.to_string().contains(code), "{err}");
            assert_eq!(s.snapshot(&a).await.unwrap(), before);
        }
        let p = s.write(&a, "good", None, b"before").await.unwrap();
        let q = s.write(&a, "bad", None, b"before").await.unwrap();
        let before = s.snapshot(&a).await.unwrap();
        let edit = |path: &str, hash: String, old: &str| ExactEdit {
            path: path.into(),
            expected_digest: hash,
            old_text: old.into(),
            new_text: "after".into(),
        };
        assert!(
            s.exact_replace(
                &a,
                before.revision,
                vec![
                    edit("good", p.hash.clone(), "before"),
                    edit("bad", q.hash.clone(), "missing")
                ]
            )
            .await
            .is_err()
        );
        assert_eq!(s.snapshot(&a).await.unwrap(), before);
        let after = s
            .exact_replace(
                &a,
                before.revision,
                vec![
                    edit("good", p.hash, "before"),
                    edit("bad", q.hash, "before"),
                ],
            )
            .await
            .unwrap();
        assert_eq!(after.revision, before.revision + 1);
    });
}
#[test]
fn local_reopen_preserves_unpublished_staging() {
    drive(async {
        let temp = tempfile::tempdir().unwrap();
        let a = Access::owner(ns("r"));
        let policy = StoragePolicy::default();
        let s = DataStore::local(&temp.path().canonicalize().unwrap(), policy.clone()).unwrap();
        s.write(&a, "nested/file", None, b"saved").await.unwrap();
        let staged = s
            .stage(&a, b"not saved", DataClass::Intermediate, 0)
            .unwrap();
        let saved = s
            .put(&a, b"checkpoint", DataClass::Checkpoint, 0)
            .await
            .unwrap();
        drop(s);
        let s = DataStore::local(&temp.path().canonicalize().unwrap(), policy).unwrap();
        assert_eq!(s.snapshot(&a).await.unwrap().files.len(), 1);
        assert!(s.artifact(&a, &staged.id).await.is_err());
        assert_eq!(s.staged(&a, &staged.id).unwrap().id, staged.id);
        assert_eq!(
            s.read_artifact(&a, &saved, 0, 10).await.unwrap().bytes,
            b"checkpoint"
        );
    });
}
#[cfg(unix)]
#[test]
fn local_rejects_symlink_hardlink_fifo_and_root_alias() {
    drive(async {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let a = Access::owner(ns("r"));
        let alias = temp.path().join("alias");
        symlink(outside.path(), &alias).unwrap();
        assert!(DataStore::local(&alias, StoragePolicy::default()).is_err());
        std::fs::remove_file(&alias).unwrap();
        let s = DataStore::local(
            &temp.path().canonicalize().unwrap(),
            StoragePolicy::default(),
        )
        .unwrap();
        s.write(&a, "a", None, b"saved").await.unwrap();
        let directory = std::fs::read_dir(temp.path())
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.is_dir())
            .unwrap();
        let state = directory.join("state");
        let original = std::fs::read(&state).unwrap();
        std::fs::remove_file(&state).unwrap();
        let protected = outside.path().join("protected");
        std::fs::write(&protected, b"protected").unwrap();
        symlink(&protected, &state).unwrap();
        assert!(s.write(&a, "b", None, b"bad").await.is_err());
        assert_eq!(std::fs::read(&protected).unwrap(), b"protected");
        std::fs::remove_file(&state).unwrap();
        std::fs::write(&state, &original).unwrap();
        std::fs::hard_link(&state, directory.join("alias-state")).unwrap();
        assert!(s.snapshot(&a).await.is_err());
        std::fs::remove_file(&state).unwrap();
        assert!(
            std::process::Command::new("/usr/bin/mkfifo")
                .arg(&state)
                .status()
                .unwrap()
                .success()
        );
        assert!(s.snapshot(&a).await.is_err());
    });
}
#[test]
fn local_two_instances_cas_and_namespace_concurrency() {
    let temp = tempfile::tempdir().unwrap();
    let s = DataStore::local(
        &temp.path().canonicalize().unwrap(),
        StoragePolicy::default(),
    )
    .unwrap();
    let other = DataStore::local(
        &temp.path().canonicalize().unwrap(),
        StoragePolicy::default(),
    )
    .unwrap();
    let a = Access::owner(ns("r"));
    drive(async {
        s.write(&a, "a", None, b"old").await.unwrap();
    });
    let mut handles = vec![];
    for store in [s, other] {
        let a = a.clone();
        handles.push(std::thread::spawn(move || {
            let mut success = false;
            drive(async {
                success = store.write(&a, "a", Some(1), b"new").await.is_ok();
            });
            success
        }));
    }
    assert_eq!(
        handles
            .into_iter()
            .map(|h| usize::from(h.join().unwrap()))
            .sum::<usize>(),
        1
    );
}
#[test]
fn pagination_search_limits_and_retention() {
    drive(async {
        let s = DataStore::memory(StoragePolicy::default());
        let a = Access::owner(ns("r"));
        s.write(&a, "a", None, b"hit\nother\nhit").await.unwrap();
        s.write(&a, "b", None, b"hit").await.unwrap();
        let p = s.list(&a, None, 1).await.unwrap();
        assert!(p.truncated);
        assert_eq!(s.list(&a, p.next, 1).await.unwrap().entries[0].path, "b");
        let p = s.search(&a, "hit", None, 1, 1024).await.unwrap();
        assert_eq!(p.entries[0].line, 1);
        assert!(p.truncated);
        assert_eq!(
            s.search(&a, "hit", p.next, 1, 1024).await.unwrap().entries[0].line,
            3
        );
        let artifact = s.put(&a, b"bytes", DataClass::Log, 0).await.unwrap();
        assert!(s.delete_artifact(&a, &artifact, 1).await.is_err());
        assert_eq!(
            s.delete_artifact(&a, &artifact, artifact.retention_until)
                .await
                .unwrap(),
            Progress::Confirmed
        );
    });
}
#[test]
fn explicit_child_sharing_is_versioned_and_revocable() {
    drive(async {
        let control = memory::MemoryStore::default();
        let account = control
            .create_account("t", Amount::new(100, 100), None)
            .await
            .unwrap();
        let parent = control
            .start(RunSpec::new("t", "parent", account.clone(), "judge"))
            .await
            .unwrap();
        let mut spec = RunSpec::new("t", "child", account, "judge");
        spec.parent = Some(parent.clone());
        let child = control.start(spec).await.unwrap();
        let child = control.get(&child).await.unwrap();
        let s = DataStore::memory(StoragePolicy::default());
        let a = Access::owner(Namespace {
            owner: parent,
            agent: "main".into(),
        });
        let v = s.write(&a, "a", None, b"one").await.unwrap();
        let read = s.share(&a, &child, "main", false, 1).await.unwrap();
        assert_eq!(
            s.read(&read, "a", v.revision, 0, 10).await.unwrap().bytes,
            b"one"
        );
        assert!(s.write(&read, "a", Some(1), b"bad").await.is_err());
        let update = s.share(&a, &child, "main", true, 1).await.unwrap();
        s.write(&update, "a", Some(1), b"two").await.unwrap();
        assert!(s.snapshot(&read).await.is_err());
        assert!(s.snapshot(&update).await.is_err());
        let read = s.share(&a, &child, "main", false, 2).await.unwrap();
        s.revoke(&a).await.unwrap();
        assert!(s.snapshot(&read).await.is_err());
    });
}
#[test]
fn artifact_basis_config_version_and_validator_are_bound() {
    drive(async {
        let s = DataStore::memory(StoragePolicy::default());
        let a = Access::owner(ns("r"));
        let r = s
            .put(&a, b"large evidence", DataClass::Intermediate, 0)
            .await
            .unwrap();
        let target =
            Target::structured(json!({"decision":"keep"}), 1, "cfg").with_artifact(r.clone());
        let evidence = s
            .validate(&a, &target, std::sync::Arc::new(Rules))
            .await
            .unwrap();
        for changed in [
            Target {
                basis_revision: 2,
                ..target.clone()
            },
            Target {
                config_version: "new".into(),
                ..target.clone()
            },
        ] {
            assert!(
                s.finalize_target(
                    &a,
                    &changed,
                    std::slice::from_ref(&evidence),
                    &[ValidatorKey::new("rules", "1")]
                )
                .await
                .is_err()
            );
        }
        assert!(
            s.finalize_target(
                &a,
                &target,
                std::slice::from_ref(&evidence),
                &[ValidatorKey::new("rules", "2")]
            )
            .await
            .is_err()
        );
        assert_eq!(
            s.artifact_status(&a, &r).await.unwrap(),
            ArtifactStatus::Saved
        );
        let id = s
            .finalize_target(&a, &target, &[evidence], &[ValidatorKey::new("rules", "1")])
            .await
            .unwrap();
        assert_eq!(
            s.artifact_status(&a, &r).await.unwrap(),
            ArtifactStatus::Finalized { completion_id: id }
        );
    });
}
#[test]
fn local_storage_failure_does_not_commit_manifest_and_capacity_is_bounded() {
    drive(async {
        let temp = tempfile::tempdir().unwrap();
        let s = DataStore::local(
            &temp.path().canonicalize().unwrap(),
            StoragePolicy::default(),
        )
        .unwrap();
        let a = Access::owner(ns("r"));
        s.write(&a, "a", None, b"saved").await.unwrap();
        let before = s.snapshot(&a).await.unwrap();
        let directory = std::fs::read_dir(temp.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.is_dir())
            .unwrap();
        let state = directory.join("state");
        std::fs::rename(&state, directory.join("backup")).unwrap();
        std::fs::create_dir(&state).unwrap();
        assert!(
            s.commit(
                &a,
                1,
                vec![Change::Write {
                    path: "b".into(),
                    expected: None,
                    bytes: b"bad".to_vec()
                }]
            )
            .await
            .is_err()
        );
        std::fs::remove_dir(&state).unwrap();
        std::fs::rename(directory.join("backup"), state).unwrap();
        assert_eq!(s.snapshot(&a).await.unwrap(), before);
        let mut policy = StoragePolicy::default();
        policy
            .classes
            .get_mut(&DataClass::Workspace)
            .unwrap()
            .max_bytes = 2;
        let s = DataStore::memory(policy);
        assert!(s.write(&a, "a", None, b"too large").await.is_err());
        assert!(s.snapshot(&a).await.unwrap().files.is_empty());
    });
}
#[test]
fn independent_namespaces_do_not_hold_a_global_data_lock() {
    let s = DataStore::memory(StoragePolicy::default());
    let mut handles = vec![];
    for i in 0..8 {
        let s = s.clone();
        handles.push(std::thread::spawn(move || {
            drive(async move {
                let a = Access::owner(ns(&format!("r{i}")));
                for rev in 0..20 {
                    s.write(&a, "a", if rev == 0 { None } else { Some(rev) }, b"x")
                        .await
                        .unwrap();
                }
            })
        }));
    }
    for handle in handles {
        handle.join().unwrap();
    }
}
#[test]
fn external_output_is_bounded_stable_and_artifact_is_immutable() {
    drive(async {
        let s = DataStore::memory(StoragePolicy::default());
        let a = Access::owner(ns("r"));
        let value = json!({"log":"x".repeat(300000)});
        let first = s
            .store_output(&a, "op", value.clone(), 32, 0)
            .await
            .unwrap();
        let second = s.store_output(&a, "op", value, 32, 1).await.unwrap();
        assert_eq!(first, second);
        let StoredOutput::External { artifact, preview } = first else {
            panic!("large output must be external")
        };
        assert_eq!(preview.bytes.len(), 32);
        assert!(preview.truncated);
        assert_eq!(
            s.read_artifact(&a, &artifact, 100, 50)
                .await
                .unwrap()
                .bytes
                .len(),
            50
        );
        let mut staged = s
            .stage(&a, b"replacement", DataClass::Intermediate, 0)
            .unwrap();
        staged.id = artifact.id.clone();
        assert_eq!(s.save(&a, staged, 0).await.unwrap_err(), Error::Conflict);
        assert_eq!(s.artifact(&a, &artifact.id).await.unwrap(), artifact);
    });
}
#[test]
fn editing_and_read_limits_reject_without_mutation() {
    drive(async {
        let policy = StoragePolicy {
            max_edits: 1,
            max_edit_bytes: 3,
            ..StoragePolicy::default()
        };
        let s = DataStore::memory(policy);
        let a = Access::owner(ns("r"));
        let v = s.write(&a, "a", None, b"abc").await.unwrap();
        let before = s.snapshot(&a).await.unwrap();
        let edit = ExactEdit {
            path: "a".into(),
            expected_digest: v.hash,
            old_text: "a".into(),
            new_text: "long".into(),
        };
        assert!(s.exact_replace(&a, 1, vec![edit.clone()]).await.is_err());
        assert!(
            s.exact_replace(&a, 1, vec![edit.clone(), edit])
                .await
                .is_err()
        );
        assert_eq!(s.snapshot(&a).await.unwrap(), before);
        assert!(s.read(&a, "a", 1, 4, 1).await.is_err());
        assert!(s.read(&a, "a", 1, 0, 300000).await.is_err());
    });
}
#[test]
fn retention_cleanup_is_explicit_versioned_and_cannot_erase_new_copy() {
    drive(async {
        let s = DataStore::memory(StoragePolicy::default());
        let a = Access::owner(ns("r"));
        s.write(&a, "a", None, b"saved").await.unwrap();
        let until = s.retain(&a, DataClass::Workspace, 0).await.unwrap();
        assert!(
            s.cleanup_class(&a, DataClass::Workspace, 1, 0)
                .await
                .is_err()
        );
        assert!(
            s.cleanup_class(&a, DataClass::Workspace, 0, until)
                .await
                .is_err()
        );
        assert_eq!(
            s.cleanup_class(&a, DataClass::Workspace, 1, until)
                .await
                .unwrap(),
            Progress::Confirmed
        );
        assert!(s.snapshot(&a).await.unwrap().files.is_empty());
        assert_eq!(s.write(&a, "a", None, b"new").await.unwrap().revision, 3);
        assert!(s.write(&a, "a", Some(1), b"old").await.is_err());
    });
}
struct RejectRules;
impl TargetValidator for RejectRules {
    fn key(&self) -> ValidatorKey {
        ValidatorKey::new("rules", "1")
    }
    fn check(&self, _: &Target) -> Result<Vec<Diagnostic>> {
        Ok(vec![Diagnostic {
            repair: DiagnosticRepair::Unknown,
            observation: None,
            schema_path: None,
            code: "MISSING_EVIDENCE".into(),
            severity: "error".into(),
            path: "decision.reason".into(),
            artifact: Some("result".into()),
            file: Some("a".into()),
            field: Some("reason".into()),
            span: Some((0, 2)),
            position: None,
            cause: "Evidence reference is absent".into(),
            hint: "Attach an existing source".into(),
        }])
    }
}
#[test]
fn host_diagnostic_classification_controls_receipt() {
    struct Rules(Vec<Diagnostic>);
    impl TargetValidator for Rules {
        fn key(&self) -> ValidatorKey {
            ValidatorKey::new("business", "1")
        }
        fn check(&self, _: &Target) -> Result<Vec<Diagnostic>> {
            Ok(self.0.clone())
        }
    }
    drive(async {
        let store = DataStore::memory(StoragePolicy::default());
        let access = Access::owner(ns("classification"));
        let target = Target::structured(json!(0), 1, "cfg");
        for repair in [
            DiagnosticRepair::Agent,
            DiagnosticRepair::Host,
            DiagnosticRepair::Operator,
            DiagnosticRepair::Unknown,
        ] {
            let mut diagnostics = schema_diagnostics(&target.value, &json!({"minimum":1})).unwrap();
            diagnostics[0].code = "BUSINESS_RULE".into();
            diagnostics[0].repair = repair;
            let evidence = store
                .validate(
                    &access,
                    &target,
                    std::sync::Arc::new(Rules(diagnostics.clone())),
                )
                .await
                .unwrap();
            assert!(!evidence.receipt.passed);
            assert_eq!(
                evidence.receipt.repairable,
                repair == DiagnosticRepair::Agent
            );
            assert_eq!(repair.agent_can_write(), evidence.receipt.repairable);
            let decoded: ValidationReceipt =
                serde_json::from_value(serde_json::to_value(&evidence.receipt).unwrap()).unwrap();
            assert_eq!(decoded, evidence.receipt);
            if repair != DiagnosticRepair::Agent {
                let mut agent = diagnostics[0].clone();
                agent.repair = DiagnosticRepair::Agent;
                diagnostics.push(agent);
                let evidence = store
                    .validate(&access, &target, std::sync::Arc::new(Rules(diagnostics)))
                    .await
                    .unwrap();
                assert!(!evidence.receipt.repairable);
            }
        }
    });
}
#[test]
fn diagnostic_locations_and_workspace_validator_cannot_self_certify() {
    drive(async {
        let s = DataStore::memory(StoragePolicy::default());
        let a = Access::owner(ns("r"));
        s.write(&a, "validator", None, b"pass everything")
            .await
            .unwrap();
        let target = Target::artifact(
            s.snapshot(&a).await.unwrap(),
            json!({"decision":"keep"}),
            1,
            "cfg",
        );
        let evidence = s
            .validate(&a, &target, std::sync::Arc::new(RejectRules))
            .await
            .unwrap();
        assert!(!evidence.receipt.passed);
        assert_eq!(evidence.receipt.diagnostics[0].span, Some((0, 2)));
        assert!(
            s.finalize_target(&a, &target, &[evidence], &[ValidatorKey::new("rules", "1")])
                .await
                .is_err()
        );
    });
}
#[test]
fn same_run_child_agent_and_tenant_boundaries() {
    drive(async {
        let s = DataStore::memory(StoragePolicy::default());
        let a = Access::owner(ns("r"));
        s.write(&a, "a", None, b"private").await.unwrap();
        let mut child = ns("r");
        child.agent = "child".into();
        assert!(
            s.snapshot(&Access::for_actor(child, a.namespace().clone()))
                .await
                .is_err()
        );
        let read = s.share_agent(&a, "child", false, 1).await.unwrap();
        assert_eq!(
            s.read(&read, "a", 1, 0, 10).await.unwrap().bytes,
            b"private"
        );
        let mut tenant = ns("r");
        tenant.owner.tenant = "other".into();
        assert!(
            s.snapshot(&Access::for_actor(tenant, a.namespace().clone()))
                .await
                .is_err()
        );
        s.put(&a, b"new artifact", DataClass::Intermediate, 0)
            .await
            .unwrap();
        assert!(s.snapshot(&read).await.is_err());
    });
}
#[test]
fn class_cleanup_reports_pending_while_retained_blobs_remain() {
    drive(async {
        let s = DataStore::memory(StoragePolicy::default());
        let a = Access::owner(ns("r"));
        let first = s.put(&a, b"first", DataClass::Log, 0).await.unwrap();
        let second = s
            .put(&a, b"second", DataClass::Log, first.retention_until)
            .await
            .unwrap();
        s.retain(&a, DataClass::Log, 0).await.unwrap();
        assert_eq!(
            s.cleanup_class(&a, DataClass::Log, 0, first.retention_until)
                .await
                .unwrap(),
            Progress::Pending
        );
        assert!(s.artifact(&a, &first.id).await.is_err());
        assert!(s.artifact(&a, &second.id).await.is_ok());
        assert_eq!(
            s.cleanup_class(&a, DataClass::Log, 0, second.retention_until)
                .await
                .unwrap(),
            Progress::Confirmed
        );
    });
}
#[test]
fn mixed_case_portable_files_work_but_case_aliases_are_rejected() {
    drive(async {
        let s = DataStore::memory(StoragePolicy::default());
        let a = Access::owner(ns("r"));
        s.write(&a, "Cargo.toml", None, b"[package]").await.unwrap();
        assert!(s.write(&a, "cargo.toml", None, b"alias").await.is_err());
        s.write(&a, "Src/main.rs", None, b"fn main(){}")
            .await
            .unwrap();
        assert!(s.write(&a, "src/lib.rs", None, b"alias").await.is_err());
        assert_eq!(s.snapshot(&a).await.unwrap().files.len(), 2);
    });
}
#[test]
fn multiple_edits_in_one_file_share_initial_digest_and_commit_once() {
    drive(async {
        let s = DataStore::memory(StoragePolicy::default());
        let a = Access::owner(ns("r"));
        let v = s.write(&a, "a", None, b"one\r\ntwo\r\n").await.unwrap();
        let edits = [("one", "first"), ("two", "second")]
            .map(|(old, new)| ExactEdit {
                path: "a".into(),
                expected_digest: v.hash.clone(),
                old_text: old.into(),
                new_text: new.into(),
            })
            .to_vec();
        let done = s.exact_replace(&a, 1, edits).await.unwrap();
        assert_eq!(done.revision, 2);
        assert_eq!(
            s.read(&a, "a", 2, 0, 100).await.unwrap().bytes,
            b"first\r\nsecond\r\n"
        );
    });
}

// Time contract: every time is Unix milliseconds on the host Clock; the fake
// `now` values below are plain numbers and nothing here waits real time.
const DAY_MS: u64 = 86_400_000;
fn policy_with_retention(retention: u64) -> StoragePolicy {
    let mut policy = StoragePolicy::default();
    for class in policy.classes.values_mut() {
        class.retention = retention;
    }
    policy
}
#[test]
fn default_retention_is_one_day_in_milliseconds() {
    let policy = StoragePolicy::default();
    assert!(policy.classes.values().all(|c| c.retention == DAY_MS));
    assert_eq!(DAY_MS, 24 * 60 * 60 * 1000);
}
#[test]
fn artifact_deletion_boundary_is_exact_in_milliseconds() {
    drive(async {
        let s = DataStore::memory(StoragePolicy::default());
        let a = Access::owner(ns("r"));
        let saved_at = 1_700_000_000_000;
        let artifact = s
            .put(&a, b"bytes", DataClass::Intermediate, saved_at)
            .await
            .unwrap();
        assert_eq!(artifact.retention_until, saved_at + DAY_MS);
        let until = artifact.retention_until;
        // Just before the boundary the artifact is retained; one second early
        // would be 1_000 here, so a seconds-based caller is also refused.
        for early in [until - 1, until - 1_000, saved_at, saved_at + DAY_MS / 1000] {
            assert_eq!(
                s.delete_artifact(&a, &artifact, early).await,
                Err(Error::PermissionDenied),
                "now={early}"
            );
        }
        // Equal to the boundary is deletable; later times stay deletable.
        assert_eq!(
            s.delete_artifact(&a, &artifact, until).await.unwrap(),
            Progress::Confirmed
        );
        assert_eq!(s.artifact_status(&a, &artifact).await, Err(Error::NotFound));
        let again = s.put(&a, b"other", DataClass::Log, saved_at).await.unwrap();
        assert_eq!(
            s.delete_artifact(&a, &again, until + 1).await.unwrap(),
            Progress::Confirmed
        );
    });
}
#[test]
fn sub_second_retention_is_not_rounded_to_seconds() {
    drive(async {
        let s = DataStore::memory(policy_with_retention(500));
        let a = Access::owner(ns("r"));
        let artifact = s.put(&a, b"bytes", DataClass::Log, 10_000).await.unwrap();
        assert_eq!(artifact.retention_until, 10_500);
        assert_eq!(
            s.delete_artifact(&a, &artifact, 10_499).await,
            Err(Error::PermissionDenied)
        );
        assert_eq!(
            s.delete_artifact(&a, &artifact, 10_500).await.unwrap(),
            Progress::Confirmed
        );
        let staged = s.stage(&a, b"pending", DataClass::Log, 20_000).unwrap();
        assert_eq!(staged.expires_at, 20_500);
    });
}
#[test]
fn finalized_artifact_needs_class_retention_plus_final_retention() {
    drive(async {
        let s = DataStore::memory(StoragePolicy::default());
        let a = Access::owner(ns("r"));
        let saved_at = 5_000;
        let r = s
            .put(&a, b"evidence", DataClass::Intermediate, saved_at)
            .await
            .unwrap();
        let target =
            Target::structured(json!({"decision":"keep"}), 1, "cfg").with_artifact(r.clone());
        let evidence = s
            .validate(&a, &target, std::sync::Arc::new(Rules))
            .await
            .unwrap();
        let id = s
            .finalize_target(&a, &target, &[evidence], &[ValidatorKey::new("rules", "1")])
            .await
            .unwrap();
        assert_eq!(
            s.artifact_status(&a, &r).await.unwrap(),
            ArtifactStatus::Finalized { completion_id: id }
        );
        // The artifact's own retention (1 day) passes, but Final adds another day.
        let own = saved_at + DAY_MS;
        let total = own + DAY_MS;
        assert_eq!(r.retention_until, own);
        for early in [own, own + 1, total - 1] {
            assert_eq!(
                s.delete_artifact(&a, &r, early).await,
                Err(Error::PermissionDenied),
                "now={early}"
            );
        }
        assert_eq!(
            s.delete_artifact(&a, &r, total).await.unwrap(),
            Progress::Confirmed
        );
    });
}
#[test]
fn retention_arithmetic_saturates_instead_of_wrapping() {
    drive(async {
        let s = DataStore::memory(StoragePolicy::default());
        let a = Access::owner(ns("r"));
        let late = u64::MAX - 10;
        let artifact = s
            .put(&a, b"bytes", DataClass::Intermediate, late)
            .await
            .unwrap();
        assert_eq!(artifact.retention_until, u64::MAX);
        assert_eq!(
            s.delete_artifact(&a, &artifact, u64::MAX - 1).await,
            Err(Error::PermissionDenied)
        );
        assert_eq!(
            s.delete_artifact(&a, &artifact, u64::MAX).await.unwrap(),
            Progress::Confirmed
        );
        // A near-overflow explicit boundary saturates and is monotonic.
        assert_eq!(
            s.retain(&a, DataClass::Workspace, late).await.unwrap(),
            u64::MAX
        );
        assert_eq!(
            s.retain(&a, DataClass::Workspace, 0).await.unwrap(),
            u64::MAX
        );
    });
}
#[test]
fn class_cleanup_boundary_uses_the_retain_time_in_milliseconds() {
    drive(async {
        let s = DataStore::memory(StoragePolicy::default());
        let a = Access::owner(ns("r"));
        s.write(&a, "a", None, b"saved").await.unwrap();
        let until = s.retain(&a, DataClass::Workspace, 2_000).await.unwrap();
        assert_eq!(until, 2_000 + DAY_MS);
        for early in [2_000, until - 1_000, until - 1] {
            assert_eq!(
                s.cleanup_class(&a, DataClass::Workspace, 1, early).await,
                Err(Error::PermissionDenied),
                "now={early}"
            );
        }
        assert_eq!(
            s.cleanup_class(&a, DataClass::Workspace, 1, until)
                .await
                .unwrap(),
            Progress::Confirmed
        );
    });
}

// SPDX-License-Identifier: Apache-2.0
use crate::*;

/// Shared artifact syntax repair scenario for memory and durable control stores.
pub async fn json_artifact_repair_contract<S: ControlStore>(store: Arc<S>) {
    use runtrell::data::*;
    struct ArtifactValidator {
        data: DataStore,
        access: Access,
    }
    impl Validator for ArtifactValidator {
        fn validate(&self, run: &RunRecord, candidate: &Value) -> Result<ValidationReceipt> {
            let artifact: ArtifactRef = serde_json::from_value(candidate.clone())
                .map_err(|_| Error::Invalid("artifact reference".into()))?;
            let bytes = block_on(self.data.read_artifact(&self.access, &artifact, 0, 1024))?;
            if bytes.truncated {
                return Err(Error::Storage);
            }
            let mut diagnostics = schema::json_artifact_diagnostics(&bytes.bytes, &artifact)?;
            for diagnostic in &mut diagnostics {
                diagnostic.repair = DiagnosticRepair::Agent;
            }
            Ok(ValidationReceipt {
                repairable: !diagnostics.is_empty(),
                hash: digest(candidate),
                basis_revision: run.spec.basis_revision,
                config_version: run.spec.config_version.clone(),
                validator: run.spec.validator.clone(),
                validator_version: run.spec.validator_version.clone(),
                passed: diagnostics.is_empty(),
                diagnostics,
            })
        }
    }
    let account = store
        .create_account("t", Amount::new(1000, 1000), None)
        .await
        .unwrap();
    let run = store
        .start(RunSpec::new(
            "t",
            "artifact-syntax",
            account.clone(),
            "repair artifact",
        ))
        .await
        .unwrap();
    let access = Access::owner(Namespace {
        owner: run.clone(),
        agent: "main".into(),
    });
    let data = DataStore::memory(StoragePolicy::default());
    let bad = data
        .put(&access, b"{\n  \"answer\": }", DataClass::Final, 0)
        .await
        .unwrap();
    let good = data
        .put(&access, b"{\n  \"answer\": 2}", DataClass::Final, 0)
        .await
        .unwrap();
    assert_eq!(
        schema::json_artifact_diagnostics(b"different", &bad),
        Err(Error::Conflict)
    );
    let model = FakeModel::scripted(vec![
        FakeModel::decision(json!(bad)),
        FakeModel::decision(json!(good)),
    ]);
    let validator = Arc::new(ArtifactValidator { data, access });
    let claim = store.claim(&run, "first", 0, 100).await.unwrap();
    let rt = Runtime {
        store: store.clone(),
        model: model.clone(),
        tools: Vec::<BusinessTool>::new(),
        policy: FakePolicy::default(),
        clock: TestClock::default(),
        validator: validator.clone(),
        finalizer: Arc::new(DecisionFinalizer),
    };
    let waiting = rt.drive(&claim).await.unwrap();
    assert_eq!(waiting.state, RunState::Waiting("validation repair".into()));
    let checkpoint = waiting.checkpoint.unwrap();
    let diagnostic = &checkpoint.payload["repair"]["receipt"]["diagnostics"][0];
    assert_eq!(diagnostic["code"], "JSON_SYNTAX");
    assert_eq!(diagnostic["artifact"], bad.id);
    assert_eq!(diagnostic["position"]["byte_offset"], 14);
    assert_eq!(checkpoint.payload["repair"]["candidate"], json!(bad));
    drop(rt);
    let claim = store.claim(&run, "second", 101, 100).await.unwrap();
    let clock = TestClock::default();
    clock.set(101);
    let rt = Runtime {
        store: store.clone(),
        model: model.clone(),
        tools: Vec::<BusinessTool>::new(),
        policy: FakePolicy::default(),
        clock,
        validator,
        finalizer: Arc::new(DecisionFinalizer),
    };
    let done = rt.drive(&claim).await.unwrap();
    assert_eq!(done.state, RunState::Terminal(Terminal::Succeeded));
    assert_eq!(done.output, Some(json!(good)));
    let ops = store.list_operations(&run).await.unwrap();
    assert_eq!(ops.len(), 2);
    assert!(
        ops.iter()
            .all(|op| op.settled && op.request.reserve.tokens > 0)
    );
    assert_eq!(store.account(&account).await.unwrap().used.tokens, 8);
    assert_eq!(model.requests.lock().unwrap().len(), 2);
    assert!(
        model.requests.lock().unwrap()[1]
            .messages
            .iter()
            .any(|m| m.text.contains("JSON_SYNTAX"))
    );
}

/// Repeated observed failures stop before budget, including alternating cycles.
/// Recreate the runtime and claim after every checkpoint, for both store adapters.
pub async fn no_progress_contract<S: ControlStore>(store: Arc<S>) {
    use runtrell::assembly::ValidationChain;
    struct NoisyValidator {
        inner: ValidationChain,
        turn: usize,
        noise: bool,
    }
    impl Validator for NoisyValidator {
        fn repair_policy(&self) -> RepairPolicy {
            self.inner.repair_policy()
        }
        fn validate(&self, run: &RunRecord, candidate: &Value) -> Result<ValidationReceipt> {
            let mut receipt = self.inner.validate(run, candidate)?;
            if self.noise {
                for diagnostic in &mut receipt.diagnostics {
                    diagnostic.cause = format!("private cause {}", self.turn);
                    diagnostic.hint = format!("private hint {}", self.turn);
                    diagnostic.file = Some("/private/sensitive".into());
                }
                if let Some(diagnostic) = receipt.diagnostics.first().cloned() {
                    let mut unsafe_code = diagnostic.clone();
                    unsafe_code.code = "private\nunsafe".into();
                    receipt.diagnostics.extend([diagnostic, unsafe_code]);
                }
                if self.turn % 2 == 1 {
                    receipt.diagnostics.reverse();
                }
            }
            Ok(receipt)
        }
    }
    for mode in [
        "same",
        "raw_normalized",
        "host_limit",
        "normalized",
        "key_order",
        "noise",
        "candidate",
        "diagnostic",
        "cycle",
    ] {
        let account = store
            .create_account("t", Amount::new(10000, 10000), None)
            .await
            .unwrap();
        let run = store
            .start(RunSpec::new(
                "t",
                &format!("no-progress-{mode}"),
                account.clone(),
                "repair",
            ))
            .await
            .unwrap();
        let replies = match mode {
            "cycle" => vec![
                "{\"answer\":0}",
                "{\"answer\":-1}",
                "{\"answer\":0}",
                "{\"answer\":-1}",
                "{\"answer\":0}",
            ],
            "raw_normalized" => vec![
                "host recovery required",
                " host  recovery\nrequired ",
                "host recovery required",
            ],
            "host_limit" => vec!["host recovery required"; 4],
            "normalized" => vec!["{\"answer\":0}", " { \"answer\" : 0 } ", "{\"answer\":0}"],
            "key_order" => vec![
                "{\"answer\":0,\"other\":1}",
                "{\"other\":1,\"answer\":0}",
                "{\"answer\":0,\"other\":1}",
            ],
            "noise" => vec!["{\"answer\":0}"; 3],
            "candidate" => vec![
                "{\"answer\":0}",
                "{\"answer\":-1}",
                "{\"answer\":-2}",
                "{\"answer\":1}",
            ],
            "diagnostic" => vec![
                "{\"answer\":0}",
                "{\"answer\":0}",
                "{\"answer\":0}",
                "{\"answer\":1}",
            ],
            _ => vec![
                "host recovery required",
                "host recovery required",
                "host recovery required",
            ],
        };
        let model = FakeModel::scripted(
            replies
                .iter()
                .map(|text| ModelReply {
                    text: (*text).into(),
                    ..FakeModel::decision(Value::Null)
                })
                .collect(),
        );
        for turn in 0..replies.len() {
            let now = turn as u64 * 101;
            let claim = store
                .claim(&run, &format!("worker-{turn}"), now, 100)
                .await
                .unwrap();
            let clock = TestClock::default();
            clock.set(now);
            let validator = ValidationChain {
                validators: vec![],
                // Same candidate with a genuinely changed rule/location.
                schema: if mode == "diagnostic" && turn == 1 {
                    json!({"required":["other"]})
                } else {
                    json!({"properties":{"answer":{"minimum":1}}})
                },
                repair: RepairPolicy {
                    max_no_progress: std::num::NonZeroU64::new(if mode == "host_limit" {
                        4
                    } else {
                        3
                    })
                    .unwrap(),
                    ..RepairPolicy::default()
                },
            };
            let rt = Runtime {
                store: store.clone(),
                model: model.clone(),
                tools: Vec::<BusinessTool>::new(),
                policy: FakePolicy::default(),
                clock,
                validator: Arc::new(NoisyValidator {
                    inner: validator,
                    turn,
                    noise: mode == "noise",
                }),
                finalizer: Arc::new(DecisionFinalizer),
            };
            let result = rt.drive(&claim).await.unwrap();
            if turn + 1 < replies.len() {
                assert_eq!(result.state, RunState::Waiting("validation repair".into()));
            } else if matches!(mode, "candidate" | "diagnostic") {
                assert_eq!(result.state, RunState::Terminal(Terminal::Succeeded));
            } else {
                let RunState::Terminal(Terminal::Failed(reason)) = result.state else {
                    panic!("no-progress did not stop: {:?}", result.state)
                };
                assert!(reason.contains("VALIDATION_NO_PROGRESS"), "{reason}");
                assert!(reason.contains(
                    if matches!(mode, "same" | "raw_normalized" | "host_limit") {
                        "JSON_SYNTAX"
                    } else {
                        "SCHEMA_VALIDATION"
                    }
                ));
                assert!(!reason.contains("host recovery"));
                assert!(!reason.contains("private"));
                assert!(!reason.contains("sensitive"));
                let cp = result.checkpoint.unwrap();
                assert_eq!(
                    cp.payload["repair"]["no_progress"]["occurrences"],
                    if mode == "cycle" { 3 } else { replies.len() }
                );
                assert!(cp.payload["last_validation"]["receipt"]["diagnostics"].is_array());
            }
        }
        assert_eq!(model.requests.lock().unwrap().len(), replies.len());
        assert!(store.account(&account).await.unwrap().used.tokens < 10000);
    }
}

/// The checked workspace bytes, rather than a completion envelope, define progress.
pub async fn artifact_progress_contract<S: ControlStore>(store: Arc<S>) {
    use runtrell::data::*;
    struct CheckedFile {
        data: DataStore,
        access: Access,
        revision_only: bool,
    }
    impl TargetValidator for CheckedFile {
        fn key(&self) -> ValidatorKey {
            ValidatorKey::new("checked-file", "1")
        }
        fn check(&self, _: &Target) -> Result<Vec<Diagnostic>> {
            let snapshot = block_on(self.data.snapshot(&self.access))?;
            let entry = snapshot
                .files
                .iter()
                .find(|f| f.path == "result.json")
                .unwrap();
            let file =
                block_on(
                    self.data
                        .read(&self.access, "result.json", entry.revision, 0, 1024),
                )?;
            let value = schema::parse_json(&file.bytes).map_err(|_| Error::Storage)?;
            let mut diagnostics = schema_diagnostics(&value, &json!({"minimum":1}))?;
            for diagnostic in &mut diagnostics {
                diagnostic.code = "BUSINESS_RULE".into();
                diagnostic.file = Some("result.json".into());
                diagnostic.repair = DiagnosticRepair::Agent;
                diagnostic.observation = Some(ArtifactObservation {
                    artifact: "result.json".into(),
                    version: if self.revision_only {
                        ArtifactVersion::Revision(entry.revision.to_string())
                    } else {
                        ArtifactVersion::ContentDigest(content_digest(&file.bytes))
                    },
                });
            }
            Ok(diagnostics)
        }
    }
    for mode in ["changed", "same", "cycle", "revision"] {
        let account = store
            .create_account("t", Amount::new(10000, 10000), None)
            .await
            .unwrap();
        let mut spec = RunSpec::new("t", &format!("artifact-progress-{mode}"), account, "finish");
        spec.validator = "checked-file".into();
        let run = store.start(spec).await.unwrap();
        let data = DataStore::memory(StoragePolicy::default());
        let access = Access::owner(Namespace {
            owner: run.clone(),
            agent: "main".into(),
        });
        let values: &[&[u8]] = match mode {
            "changed" | "revision" => &[b"0", b"-1", b"-2", b"1"],
            "cycle" => &[b"0", b"-1", b"0", b"-1", b"0"],
            _ => &[b"0", b"0", b"0"],
        };
        let mut revision = None;
        for (turn, bytes) in values.iter().enumerate() {
            revision = Some(
                data.write(&access, "result.json", revision, bytes)
                    .await
                    .unwrap()
                    .revision,
            );
            let now = turn as u64 * 101;
            let claim = store.claim(&run, "worker", now, 100).await.unwrap();
            // Fresh model/runtime on every checkpoint; envelope noise must not help.
            let model = FakeModel::scripted(vec![FakeModel::decision(if mode == "same" {
                json!({"complete":true,"message":turn})
            } else {
                json!({"complete":true})
            })]);
            let clock = TestClock::default();
            clock.set(now);
            let rt = Runtime {
                store: store.clone(),
                model,
                tools: Vec::<BusinessTool>::new(),
                policy: FakePolicy::default(),
                clock,
                validator: Arc::new(DataValidator {
                    store: data.clone(),
                    access: access.clone(),
                    workspace: true,
                    artifacts: vec![],
                    validator: CheckedFile {
                        data: data.clone(),
                        access: access.clone(),
                        revision_only: mode == "revision",
                    },
                }),
                finalizer: Arc::new(DecisionFinalizer),
            };
            let result = rt.drive(&claim).await.unwrap();
            if turn + 1 < values.len() {
                assert_eq!(
                    result.state,
                    RunState::Waiting("validation repair".into()),
                    "{mode}/{turn}"
                );
            } else if matches!(mode, "changed" | "revision") {
                assert_eq!(
                    result.state,
                    RunState::Terminal(Terminal::Succeeded),
                    "{mode}"
                );
            } else {
                assert!(
                    matches!(result.state, RunState::Terminal(Terminal::Failed(ref reason)) if reason.contains("VALIDATION_NO_PROGRESS")),
                    "{mode}: {:?}",
                    result.state
                );
                assert_eq!(
                    result.checkpoint.unwrap().payload["repair"]["no_progress"]["occurrences"],
                    3
                );
            }
        }
        assert!(
            store
                .list_operations(&run)
                .await
                .unwrap()
                .iter()
                .all(|op| op.settled)
        );
    }
}

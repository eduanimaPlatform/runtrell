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
            let diagnostics = schema::json_artifact_diagnostics(&bytes.bytes, &artifact)?;
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

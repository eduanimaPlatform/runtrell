// SPDX-License-Identifier: Apache-2.0
use runtrell::{assembly::*, data::*, file_tools::*, memory::MemoryStore, *};
use runtrell_testkit::*;
use serde_json::json;
use std::{collections::BTreeSet, sync::Arc};
#[test]
fn heterogeneous_tools_and_all_configuration_failures() {
    let store = DataStore::memory(StoragePolicy::default());
    let access = Access::owner(Namespace {
        owner: RunHandle {
            tenant: "t".into(),
            id: "r".into(),
        },
        agent: "main".into(),
    });
    let tools = [
        RegisteredTool::new(FileTool {
            store,
            access,
            action: FileAction::List,
        }),
        RegisteredTool::new(BusinessTool::new("search")),
    ];
    assert_eq!(tools[0].descriptor().name, "files.list");
    assert_eq!(tools[1].descriptor().name, "search");
    let mut descriptor = tools[0].descriptor();
    descriptor.timeout = 0;
    descriptor.input_schema = json!({"unsupported":true});
    let config = AssemblyConfig {
        tools: vec![descriptor.clone(), descriptor],
        validators: vec![
            ValidatorKey::new("check", ""),
            ValidatorKey::new("check", "1"),
        ],
        required_validators: vec!["missing".into(), "missing".into()],
        available_capabilities: BTreeSet::new(),
        required_capabilities: BTreeSet::from(["network".into()]),
        bounds: vec![SettingBound {
            name: "output".into(),
            value: 0,
            minimum: 1,
            maximum: 16,
        }],
    };
    let codes: BTreeSet<_> = config
        .diagnostics()
        .iter()
        .map(|d| d.code.clone())
        .collect();
    for code in [
        "TOOL_NAME_CONFLICT",
        "TOOL_CONFIG_INVALID",
        "SCHEMA_INVALID",
        "VALIDATOR_NAME_CONFLICT",
        "VALIDATOR_CONFIG_INVALID",
        "VALIDATOR_NOT_REGISTERED",
        "CAPABILITY_MISSING",
        "SETTING_OUT_OF_RANGE",
    ] {
        assert!(codes.contains(code), "{code}");
    }
    let mut first = json!({});
    first["z"] = json!({"b":1,"a":2});
    first["a"] = json!(3);
    let second = json!({"a":3,"z":{"a":2,"b":1}});
    assert_eq!(configuration_digest(&first), configuration_digest(&second));
    assert_ne!(
        configuration_digest(&json!([1, 2])),
        configuration_digest(&json!([2, 1]))
    );
}
#[test]
fn chain_verifies_each_receipt_before_using_diagnostics() {
    block_on(async {
        let store = MemoryStore::default();
        let account = store
            .create_account("t", Amount::new(100, 100), None)
            .await
            .unwrap();
        let run = store
            .start(RunSpec::new("t", "r", account, "judge"))
            .await
            .unwrap();
        let run = store.get(&run).await.unwrap();
        let value = json!({"decision":"done"});
        let receipt = DecisionValidator::default().validate(&run, &value).unwrap();
        for mutation in 0..5 {
            let mut forged = receipt.clone();
            match mutation {
                0 => forged.hash = "other".into(),
                1 => forged.basis_revision += 1,
                2 => forged.config_version = "other".into(),
                3 => forged.validator = "other".into(),
                _ => forged.validator_version = "other".into(),
            }
            assert_eq!(
                verify_receipt(&run, &value, &forged, &ValidatorKey::new("host", "1")),
                Err(Error::Conflict)
            );
        }
        assert!(
            CheckedFinalizer(&DecisionFinalizer)
                .finalize(&run, value.clone(), receipt.clone())
                .is_ok()
        );
        assert_eq!(
            CheckedFinalizer(&ChangedFinalizer).finalize(&run, value.clone(), receipt.clone()),
            Err(Error::Conflict)
        );
        let chain = ValidationChain {
            validators: vec![VersionedValidator {
                key: ValidatorKey::new("host", "1"),
                validator: Arc::new(DecisionValidator::default()),
            }],
            schema: json!({"type":"object"}),
            repair: RepairPolicy::default(),
            repairable_codes: BTreeSet::new(),
        };
        assert!(chain.validate(&run, &value).unwrap().passed);
        // Typed host validators must not turn a schema failure into an error
        // while trying to consume a candidate that has not passed its schema.
        let invalid = chain.validate(&run, &json!([])).unwrap();
        assert!(!invalid.passed && invalid.repairable);
        assert_eq!(invalid.diagnostics[0].path, "");
    });
}

struct ChangedFinalizer;
impl Finalizer for ChangedFinalizer {
    fn finalize(
        &self,
        _: &RunRecord,
        output: serde_json::Value,
        mut receipt: ValidationReceipt,
    ) -> Result<Completion> {
        receipt.diagnostics.clear();
        receipt.validator_version = "changed".into();
        Ok(Completion {
            id: "completion".into(),
            output,
            receipt,
        })
    }
}

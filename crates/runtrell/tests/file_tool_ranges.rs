// SPDX-License-Identifier: Apache-2.0
use runtrell::{data::*, file_tools::*, *};
use serde_json::{Value, json};

fn access() -> Access {
    Access::owner(Namespace {
        owner: RunHandle {
            tenant: "t".into(),
            id: "r".into(),
        },
        agent: "main".into(),
    })
}
fn tool(store: &DataStore, action: FileAction) -> FileTool {
    FileTool {
        store: store.clone(),
        access: access(),
        action,
    }
}
fn accepts(tool: &FileTool, value: &Value) -> bool {
    validate_schema(value, &tool.descriptor().input_schema).is_ok()
}

#[tokio::test]
async fn policy_boundaries_match_schema_and_store() {
    // Deliberately nondefault: hard-coded schema limits must fail this test.
    for (max_entries, max_read, max_edits) in [(2, 256, 2), (3, 512, 3)] {
        let policy = StoragePolicy {
            max_entries,
            max_read,
            max_edits,
            ..Default::default()
        };
        let store = DataStore::memory(policy);
        let a = access();
        let file = store.write(&a, "a", None, b"alpha").await.unwrap();
        let list = tool(&store, FileAction::List);
        for limit in [0, 1, max_entries, max_entries + 1] {
            let input = json!({"limit":limit});
            assert_eq!(
                accepts(&list, &input),
                store.list(&a, None, limit).await.is_ok()
            );
        }
        for action in [FileAction::Read, FileAction::ArtifactRead] {
            let artifact = store
                .put(&a, b"alpha", DataClass::Intermediate, 0)
                .await
                .unwrap();
            for length in [0, 1, max_read, max_read + 1] {
                let (input, result) = if matches!(action, FileAction::Read) {
                    (
                        json!({"path":"a","revision":file.revision,"offset":0,"length":length}),
                        store.read(&a, "a", file.revision, 0, length).await,
                    )
                } else {
                    (
                        json!({"artifact":artifact,"offset":0,"length":length}),
                        store.read_artifact(&a, &artifact, 0, length).await,
                    )
                };
                assert_eq!(accepts(&tool(&store, action), &input), result.is_ok());
            }
        }
        let search = tool(&store, FileAction::Search);
        for query in [
            String::new(),
            "z".into(),
            "z".repeat(MAX_SEARCH_QUERY_BYTES),
            "z".repeat(MAX_SEARCH_QUERY_BYTES + 1),
        ] {
            for limit in [0, 1, max_entries, max_entries + 1] {
                for max_bytes in [
                    MIN_SEARCH_BYTES - 1,
                    MIN_SEARCH_BYTES,
                    max_read,
                    max_read + 1,
                ] {
                    let input = json!({"query":query,"limit":limit,"max_bytes":max_bytes});
                    assert_eq!(
                        accepts(&search, &input),
                        store
                            .search(&a, &query, None, limit, max_bytes)
                            .await
                            .is_ok(),
                        "{input}"
                    );
                }
            }
        }
        for count in [0, 1, max_edits, max_edits + 1] {
            let fresh = DataStore::memory(store.policy().clone());
            let file = fresh.write(&a, "a", None, b"a").await.unwrap();
            let edits: Vec<_> = (0..count)
                .map(|_| ExactEdit {
                    path: "a".into(),
                    expected_digest: file.hash.clone(),
                    old_text: "a".into(),
                    new_text: "a".into(),
                })
                .collect();
            let input = json!({"expected_revision":1,"edits":edits});
            assert_eq!(
                accepts(&tool(&fresh, FileAction::ExactReplace), &input),
                fresh.exact_replace(&a, 1, edits).await.is_ok()
            );
        }
    }
}

#[test]
fn all_path_schemas_match_canonical_path_checks() {
    let store = DataStore::memory(StoragePolicy::default());
    let full = format!("{}/a", vec!["a".repeat(127); 8].join("/"));
    let paths = [
        "a".into(),
        ".config/a_B-1.txt".into(),
        String::new(),
        ".".into(),
        "..".into(),
        "a/../b".into(),
        "/a".into(),
        "a/".into(),
        "a//b".into(),
        "é".into(),
        "a\n".into(),
        "a".repeat(MAX_PATH_COMPONENT_BYTES),
        "a".repeat(MAX_PATH_COMPONENT_BYTES + 1),
        format!("{}/{}", vec!["a".repeat(127); 7].join("/"), "a".repeat(128)),
        full[..MAX_LOGICAL_PATH_BYTES].into(),
        full,
    ];
    for action in [
        FileAction::Read,
        FileAction::Write,
        FileAction::Delete,
        FileAction::ExactReplace,
    ] {
        let schema = tool(&store, action).descriptor().input_schema;
        let path_schema = if matches!(action, FileAction::ExactReplace) {
            &schema["properties"]["edits"]["items"]["properties"]["path"]
        } else {
            &schema["properties"]["path"]
        };
        for path in &paths {
            assert_eq!(
                validate_schema(&json!(path), path_schema).is_ok(),
                logical_path(path).is_ok(),
                "{path:?}"
            );
        }
    }
}

#[test]
fn text_bounds_come_from_policy_without_changing_byte_semantics() {
    let mut policy = StoragePolicy {
        max_edit_bytes: 200,
        ..Default::default()
    };
    policy
        .classes
        .get_mut(&DataClass::Workspace)
        .unwrap()
        .max_bytes = 300;
    let store = DataStore::memory(policy);
    let write = tool(&store, FileAction::Write).descriptor();
    let replace = tool(&store, FileAction::ExactReplace).descriptor();
    assert_eq!(write.version, "runtrell-files-3");
    assert_eq!(write.input_schema["properties"]["text"]["maxLength"], 300);
    let fields = &replace.input_schema["properties"]["edits"]["items"]["properties"];
    assert_eq!(fields["old_text"]["maxLength"], 200);
    assert_eq!(fields["old_text"]["minLength"], 1);
    assert_eq!(fields["new_text"]["maxLength"], 200);
    assert!(validate_schema(&json!(""), &fields["new_text"]).is_ok());
    assert!(validate_schema(&json!(""), &fields["old_text"]).is_err());
}

#[tokio::test]
async fn text_capacity_and_aggregate_edit_bytes_are_not_character_counts() {
    let mut policy = StoragePolicy {
        max_edit_bytes: 5,
        ..Default::default()
    };
    policy
        .classes
        .get_mut(&DataClass::Workspace)
        .unwrap()
        .max_bytes = 20;
    for text in [
        "x".repeat(20),
        "x".repeat(21),
        "é".repeat(10),
        "é".repeat(11),
    ] {
        let store = DataStore::memory(policy.clone());
        let result = store.write(&access(), "a", None, text.as_bytes()).await;
        assert_eq!(result.is_ok(), text.len() <= 20);
        // Unicode maxLength is a necessary bound; byte capacity remains authoritative.
        if text.is_ascii() {
            assert_eq!(
                accepts(
                    &tool(&store, FileAction::Write),
                    &json!({"path":"a","expected_revision":null,"text":text})
                ),
                result.is_ok()
            );
        }
    }
    for new_text in ["xxx", "xxxx", "é", "éé"] {
        let store = DataStore::memory(policy.clone());
        let file = store
            .write(&access(), "a", None, "é".as_bytes())
            .await
            .unwrap();
        let edits = vec![ExactEdit {
            path: "a".into(),
            expected_digest: file.hash,
            old_text: "é".into(),
            new_text: new_text.into(),
        }];
        let result = store.exact_replace(&access(), 1, edits).await;
        assert_eq!(result.is_ok(), 2 + new_text.len() <= policy.max_edit_bytes);
        if let Err(Error::Invalid(code)) = result {
            assert_eq!(code, "EDIT_LIMIT");
        }
    }
}

async fn invoke(tool: &FileTool, mut input: Value) -> ExternalResult {
    let control = memory::MemoryStore::default();
    let account = control
        .create_account("t", Amount::new(100, 100), None)
        .await
        .unwrap();
    let run = control
        .start(RunSpec::new("t", "r", account, "test"))
        .await
        .unwrap();
    let claim = control.claim(&run, "worker", 0, 100).await.unwrap();
    // The test store chooses a generated Run ID; bind the tool to that actual actor.
    let mut tool = tool.clone();
    tool.access = Access::owner(Namespace {
        owner: run.clone(),
        agent: "main".into(),
    });
    tool.store
        .write(&tool.access, "a", None, b"alpha")
        .await
        .unwrap();
    if matches!(tool.action, FileAction::ArtifactRead) {
        input["artifact"] = json!(
            tool.store
                .put(&tool.access, b"alpha", DataClass::Intermediate, 0)
                .await
                .unwrap()
        );
    }
    let d = tool.descriptor();
    let context = ToolContext {
        recovery_proof: None,
        claim,
        principal: control.get(&run).await.unwrap().spec.principal,
        operation: "test".into(),
        attempt: 1,
        deadline: None,
        permission: d.permission,
        account_binding: d.account,
    };
    tool.invoke(&context, input).await.unwrap()
}

#[tokio::test]
async fn runtime_argument_bounds_have_safe_policy_hints() {
    let policy = StoragePolicy {
        max_entries: 7,
        max_read: 512,
        max_edits: 2,
        ..Default::default()
    };
    let store = DataStore::memory(policy);
    for (action, input, code, bounds) in [
        (
            FileAction::List,
            json!({"limit":0}),
            "PAGE_LIMIT",
            vec!["1..=7"],
        ),
        (
            FileAction::Search,
            json!({"query":"","limit":1,"max_bytes":128}),
            "SEARCH_LIMIT",
            vec!["1..=4096", "1..=7", "128..=512"],
        ),
        (
            FileAction::ExactReplace,
            json!({"expected_revision":0,"edits":[]}),
            "EDIT_LIMIT",
            vec!["1..=2", "1048576"],
        ),
        (
            FileAction::Read,
            json!({"path":"a","revision":1,"offset":0,"length":0}),
            "RANGE_LIMIT",
            vec!["1..=512"],
        ),
        (
            FileAction::ArtifactRead,
            json!({"offset":0,"length":513}),
            "RANGE_LIMIT",
            vec!["1..=512"],
        ),
        (
            FileAction::Read,
            json!({"path":"../private-value","revision":1,"offset":0,"length":1}),
            "PATH_NOT_CANONICAL",
            vec!["1..=1024", "1..=128"],
        ),
    ] {
        let ExternalResult::Failed {
            code: actual,
            repair_hint: Some(hint),
            ..
        } = invoke(&tool(&store, action), input).await
        else {
            panic!("expected repairable {code}")
        };
        assert_eq!(actual, code);
        assert!(!hint.contains("private-value"));
        for bound in bounds {
            assert!(hint.contains(bound), "{hint}");
        }
    }
}

#[tokio::test]
async fn storage_capacity_and_output_limits_remain_fatal() {
    let mut policy = StoragePolicy::default();
    policy
        .classes
        .get_mut(&DataClass::Workspace)
        .unwrap()
        .max_bytes = 5;
    let store = DataStore::memory(policy);
    let cases = [
        (
            FileAction::Write,
            json!({"path":"b","expected_revision":null,"text":"x"}),
            "CAPACITY",
        ),
        (
            FileAction::Write,
            json!({"path":"b","expected_revision":null,"text":"x".repeat(store.policy().max_edit_bytes)}),
            "INPUT_LIMIT",
        ),
    ];
    for (action, input, code) in cases {
        let ExternalResult::Failed {
            code: actual,
            repair_hint,
            ..
        } = invoke(&tool(&store, action), input).await
        else {
            panic!("expected fatal {code}")
        };
        assert!(actual.contains(code));
        assert!(repair_hint.is_none());
    }
}

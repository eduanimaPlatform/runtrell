// SPDX-License-Identifier: Apache-2.0
use runtrell::{data::*, file_tools::*, *};
use serde_json::{Value, json};

async fn setup(action: FileAction, policy: StoragePolicy) -> (FileTool, ToolContext) {
    let control = memory::MemoryStore::default();
    let account = control
        .create_account("tenant", Amount::new(100, 100), None)
        .await
        .unwrap();
    let run = control
        .start(RunSpec::new("tenant", "run", account, "test"))
        .await
        .unwrap();
    let claim = control.claim(&run, "worker", 0, 100).await.unwrap();
    let tool = FileTool {
        store: DataStore::memory(policy),
        access: Access::owner(Namespace {
            owner: run.clone(),
            agent: "main".into(),
        }),
        action,
    };
    let descriptor = tool.descriptor();
    let context = ToolContext {
        recovery_proof: None,
        claim,
        principal: control.get(&run).await.unwrap().spec.principal,
        operation: "read".into(),
        attempt: 1,
        deadline: None,
        permission: descriptor.permission,
        account_binding: descriptor.account,
    };
    (tool, context)
}

async fn save(tool: &FileTool, media_type: &str, bytes: &[u8]) -> ArtifactRef {
    let staged = tool
        .store
        .stage_named(
            &tool.access,
            ArtifactSpec {
                id: "source".into(),
                media_type: media_type.into(),
                class: DataClass::Intermediate,
            },
            bytes,
            0,
        )
        .unwrap();
    tool.store.save(&tool.access, staged, 0).await.unwrap()
}

async fn artifact_read(
    tool: &FileTool,
    context: &ToolContext,
    artifact: &ArtifactRef,
    length: usize,
) -> ExternalResult {
    tool.invoke(
        context,
        json!({"artifact":artifact,"offset":0,"length":length}),
    )
    .await
    .unwrap()
}

fn applied_text(result: ExternalResult) -> Value {
    let ExternalResult::Applied { output, .. } = result else {
        panic!("expected text output")
    };
    assert!(output.get("bytes").is_none(), "raw bytes reached model");
    output
}

#[tokio::test]
async fn binary_artifacts_return_typed_failure_without_bytes() {
    for (mime, bytes) in [
        ("application/pdf", b"%PDF-1.7".as_slice()),
        ("image/png", b"\x89PNG\r\n\x1a\n".as_slice()),
        ("application/octet-stream", b"unknown".as_slice()),
    ] {
        let (tool, context) = setup(FileAction::ArtifactRead, StoragePolicy::default()).await;
        let artifact = save(&tool, mime, bytes).await;
        let result = artifact_read(&tool, &context, &artifact, bytes.len()).await;
        let ExternalResult::Failed {
            code,
            repair_hint: Some(hint),
            ..
        } = result
        else {
            panic!("binary {mime} reached model as success")
        };
        assert_eq!(code, "BINARY_ARTIFACT");
        assert!(hint.contains("document.view"));
        assert!(hint.contains(mime));
        assert!(hint.contains(&artifact.hash));
        assert!(hint.contains(&artifact.bytes.to_string()));
        assert!(!hint.contains("%PDF"));
        assert!(!hint.contains("PNG"));
        assert!(serde_json::to_vec(&hint).unwrap().len() < 4096);
    }
}

#[tokio::test]
async fn text_artifacts_return_utf8_string_and_reject_invalid_ranges() {
    for mime in [
        "text/plain",
        "text/markdown",
        "application/json",
        "application/vnd.test+json",
    ] {
        let (tool, context) = setup(FileAction::ArtifactRead, StoragePolicy::default()).await;
        let artifact = save(&tool, mime, "日本語 alpha".as_bytes()).await;
        let output =
            applied_text(artifact_read(&tool, &context, &artifact, artifact.bytes as usize).await);
        assert_eq!(output["text"], "日本語 alpha");
        assert_eq!(output["offset"], 0);
        assert_eq!(output["total"], artifact.bytes);
        assert_eq!(output["truncated"], false);
        let result = artifact_read(&tool, &context, &artifact, 1).await;
        assert!(matches!(result, ExternalResult::Failed { code, .. } if code == "NON_UTF8_TEXT"));
    }
    let (tool, context) = setup(FileAction::ArtifactRead, StoragePolicy::default()).await;
    let invalid = save(&tool, "text/plain", b"\xffbad").await;
    let result = artifact_read(&tool, &context, &invalid, invalid.bytes as usize).await;
    assert!(matches!(result, ExternalResult::Failed { code, .. } if code == "NON_UTF8_TEXT"));

    let (tool, context) = setup(FileAction::ArtifactRead, StoragePolicy::default()).await;
    let disguised = save(&tool, "text/plain", b"plain\0binary").await;
    let result = artifact_read(&tool, &context, &disguised, disguised.bytes as usize).await;
    assert!(matches!(result, ExternalResult::Failed { code, .. } if code == "BINARY_TEXT"));
}

#[tokio::test]
async fn large_text_and_workspace_read_stay_text_only_and_json_bounded() {
    let policy = StoragePolicy::default();
    let length = policy.max_read.min(200_000);
    let (artifact_tool, context) = setup(FileAction::ArtifactRead, policy.clone()).await;
    let text = "a".repeat(length);
    let artifact = save(&artifact_tool, "text/plain", text.as_bytes()).await;
    let result = artifact_read(&artifact_tool, &context, &artifact, length).await;
    let output = applied_text(result);
    assert_eq!(output["text"].as_str().unwrap().len(), length);
    assert!(
        serde_json::to_vec(&output).unwrap().len() <= artifact_tool.descriptor().max_output_bytes
    );

    let (file_tool, context) = setup(FileAction::Read, policy).await;
    let file = file_tool
        .store
        .write(&file_tool.access, "notes.txt", None, b"hello")
        .await
        .unwrap();
    let output = applied_text(
        file_tool
            .invoke(
                &context,
                json!({"path":"notes.txt","revision":file.revision,"offset":0,"length":5}),
            )
            .await
            .unwrap(),
    );
    assert_eq!(output["text"], "hello");
    let raw = file_tool
        .store
        .write(&file_tool.access, "binary", None, b"\xff")
        .await
        .unwrap();
    let failed = file_tool
        .invoke(
            &context,
            json!({"path":"binary","revision":raw.revision,"offset":0,"length":1}),
        )
        .await
        .unwrap();
    assert!(matches!(failed, ExternalResult::Failed { code, .. } if code == "NON_UTF8_TEXT"));
    let control = file_tool
        .store
        .write(&file_tool.access, "control", None, b"hello\0")
        .await
        .unwrap();
    let failed = file_tool
        .invoke(
            &context,
            json!({"path":"control","revision":control.revision,"offset":0,"length":6}),
        )
        .await
        .unwrap();
    assert!(matches!(failed, ExternalResult::Failed { code, .. } if code == "BINARY_TEXT"));
}

#[tokio::test]
async fn forged_binary_reference_is_rejected_before_media_classification() {
    let (tool, context) = setup(FileAction::ArtifactRead, StoragePolicy::default()).await;
    let artifact = save(&tool, "application/pdf", b"%PDF").await;
    let mut forged = artifact.clone();
    forged.hash = "forged".into();
    let result = artifact_read(&tool, &context, &forged, 4).await;
    assert!(
        matches!(result, ExternalResult::Failed { code, repair_hint: None, .. } if code != "BINARY_ARTIFACT")
    );

    let mut foreign = artifact;
    foreign.owner.owner.tenant = "other".into();
    let result = artifact_read(&tool, &context, &foreign, 4).await;
    assert!(
        matches!(result, ExternalResult::Failed { code, repair_hint: None, .. } if code != "BINARY_ARTIFACT")
    );
}

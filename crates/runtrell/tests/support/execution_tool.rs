// SPDX-License-Identifier: Apache-2.0
use runtrell::{data::*, execution::*, memory::MemoryStore, *};
use serde_json::json;
use std::sync::Arc;
struct WallClock;
impl Clock for WallClock {
    fn now(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }
    async fn wait_until(&self, deadline: u64) {
        tokio::time::sleep(std::time::Duration::from_millis(
            deadline.saturating_sub(self.now()),
        ))
        .await;
    }
}
struct Allow;
impl Policy for Allow {
    fn authorize(&self, _: &RunRecord, _: &str, _: &str) -> Result<()> {
        Ok(())
    }
}
pub async fn exercise(backend: impl ExecutionBackend, limits: ExecutionLimits) {
    let control = Arc::new(MemoryStore::default());
    let account = control
        .create_account("test", Amount::new(1000, 1000), None)
        .await
        .unwrap();
    let owner = control
        .start(RunSpec::new("test", "real-tool", account, "execute"))
        .await
        .unwrap();
    let claim = control
        .claim(&owner, "worker", WallClock.now(), 60000)
        .await
        .unwrap();
    let workspace = DataStore::memory(StoragePolicy::default());
    let access = Access::owner(Namespace {
        owner,
        agent: "main".into(),
    });
    workspace
        .write(&access, "input", None, b"copied")
        .await
        .unwrap();
    workspace
        .write(&access, "hidden", None, b"private")
        .await
        .unwrap();
    let ledger = MemoryExecutionLedger::default();
    let tool = ExecutionTool::new(
        control,
        workspace.clone(),
        access.clone(),
        ledger.clone(),
        backend,
        (Allow, WallClock),
        ExecutionToolConfig {
            executables: [("/bin/sh".into(), ArgumentPolicy::Any)]
                .into_iter()
                .collect(),
            limits,
            requires_descendants: false,
            reserve: Amount::new(0, 100),
            summary_bytes: 64,
        },
    );
    assert!(tool.diagnostics().is_empty());
    let context = ToolContext {
        recovery_proof: None,
        claim,
        principal: "host".into(),
        operation: "real-tool".into(),
        attempt: 1,
        deadline: Some(WallClock.now() + 30000),
        permission: "execute".into(),
        account_binding: "execution".into(),
    };
    let input = json!({"program":{"Process":{"executable":"/bin/sh","args":["-c","test ! -e hidden && { IFS= read -r value < input; printf '%s' \"$value\" > output; printf executed; }"]}},"files":["input"],"outputs":["output"]});
    let mut result = tool.invoke(&context, input.clone()).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        while matches!(result, ExternalResult::Accepted { .. }) {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            if let Lookup::Found(found) = tool.lookup(&context, input.clone()).await.unwrap() {
                result = found;
            }
        }
    })
    .await
    .unwrap();
    let ExternalResult::Applied { output, .. } = result else {
        panic!("execution did not return a result: {result:?}")
    };
    assert_eq!(output["saved"], true, "{output}");
    assert_eq!(output["exit"], 0);
    assert_eq!(output["stdout"]["text"], "executed");
    let snapshot = workspace.snapshot(&access).await.unwrap();
    let file = snapshot.files.iter().find(|f| f.path == "output").unwrap();
    assert_eq!(
        workspace
            .read(&access, "output", file.revision, 0, 64)
            .await
            .unwrap()
            .bytes,
        b"copied"
    );
    let artifact: ArtifactRef = serde_json::from_value(output["artifact"].clone()).unwrap();
    assert!(
        !workspace
            .read_artifact(&access, &artifact, 0, 1024)
            .await
            .unwrap()
            .bytes
            .is_empty()
    );
    assert!(
        ledger
            .get(&context.claim.run, &context.operation)
            .await
            .unwrap()
            .reclaimed()
    );
    assert!(ledger.pending().await.unwrap().is_empty());
}

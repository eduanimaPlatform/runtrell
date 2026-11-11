// SPDX-License-Identifier: Apache-2.0
use runtrell::{data::*, *};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

fn access(run: &str, agent: &str) -> Access {
    Access::owner(Namespace {
        owner: RunHandle {
            tenant: "tenant".into(),
            id: run.into(),
        },
        agent: agent.into(),
    })
}

#[derive(Default)]
struct External {
    objects: Mutex<BTreeMap<String, (u64, Vec<u8>)>>,
    fail_after_commit: AtomicBool,
}
impl NamespaceBackend for External {
    fn load(&self, key: &str) -> Result<(String, Option<Vec<u8>>)> {
        Ok(self
            .objects
            .lock()
            .unwrap()
            .get(key)
            .map(|(v, bytes)| (v.to_string(), Some(bytes.clone())))
            .unwrap_or(("0".into(), None)))
    }
    fn compare_exchange(&self, key: &str, expected: &str, payload: &[u8]) -> Result<()> {
        let mut objects = self.objects.lock().unwrap();
        let version = objects.get(key).map_or(0, |(v, _)| *v);
        if expected != version.to_string() {
            return Err(Error::Conflict);
        }
        objects.insert(key.into(), (version + 1, payload.to_vec()));
        if self.fail_after_commit.swap(false, Ordering::SeqCst) {
            return Err(Error::Storage);
        }
        Ok(())
    }
}

struct Accept;
impl TargetValidator for Accept {
    fn key(&self) -> ValidatorKey {
        ValidatorKey::new("erase-test", "1")
    }
    fn check(&self, _: &Target) -> Result<Vec<Diagnostic>> {
        Ok(vec![])
    }
}

async fn exercise(store: &DataStore, inspect: impl Fn() -> Vec<Vec<u8>>) {
    let a = access("erase", "first");
    let b = access("erase", "second");
    let other = access("keep", "first");
    for (access, body) in [
        (&a, b"private-first".as_slice()),
        (&b, b"private-second".as_slice()),
        (&other, b"private-other".as_slice()),
    ] {
        store.write(access, "file", None, body).await.unwrap();
    }
    assert_eq!(
        store.read(&b, "file", 1, 0, 100).await.unwrap().bytes,
        b"private-second"
    );
    let staged = store
        .stage_named(
            &b,
            ArtifactSpec {
                id: "staged-secret".into(),
                media_type: "text/plain".into(),
                class: DataClass::Intermediate,
            },
            b"private-staged",
            0,
        )
        .unwrap();
    let saved = store
        .put(&a, b"private-artifact", DataClass::Intermediate, 0)
        .await
        .unwrap();
    store
        .append_context(
            &a,
            ContextItem {
                source: Source::Input,
                value: serde_json::json!("private-context"),
            },
        )
        .await
        .unwrap();
    let grant = store.share_agent(&a, "reader", false, 1).await.unwrap();
    let target = Target::structured(serde_json::json!("private-evidence"), 1, "cfg");
    let evidence = store.validate(&a, &target, Arc::new(Accept)).await.unwrap();
    store
        .finalize_target(
            &a,
            &target,
            &[evidence],
            &[ValidatorKey::new("erase-test", "1")],
        )
        .await
        .unwrap();
    store.erase_run(&a.namespace().owner).unwrap();
    store.erase_run(&a.namespace().owner).unwrap();
    let new_agent = access("erase", "new-agent");
    for access in [&a, &b, &new_agent] {
        assert_eq!(store.snapshot(access).await.unwrap_err(), Error::Closed);
        assert_eq!(
            store
                .write(access, "file", None, b"resurrect")
                .await
                .unwrap_err(),
            Error::Closed
        );
    }
    assert_eq!(store.staged(&b, &staged.id).unwrap_err(), Error::Closed);
    assert_eq!(
        store.artifact(&a, &saved.id).await.unwrap_err(),
        Error::Closed
    );
    assert_eq!(store.context(&a).await.unwrap_err(), Error::Closed);
    assert_eq!(store.snapshot(&grant).await.unwrap_err(), Error::Closed);
    assert_eq!(
        store.read(&other, "file", 1, 0, 100).await.unwrap().bytes,
        b"private-other"
    );
    let raw = inspect();
    if !raw.is_empty() {
        assert_eq!(raw.len(), 2);
        let documents: Vec<serde_json::Value> = raw
            .iter()
            .map(|bytes| serde_json::from_slice(bytes).unwrap())
            .collect();
        assert!(documents.iter().any(|value| {
            value["erased"] == true && value["namespaces"].as_object().unwrap().is_empty()
        }));
        assert!(documents.iter().any(|value| {
            value["erased"] == false && value["namespaces"].as_object().unwrap().len() == 1
        }));
    }
    assert!(raw.iter().all(|bytes| {
        !bytes
            .windows(b"private-first".len())
            .any(|w| w == b"private-first")
    }));
    assert!(raw.iter().all(|bytes| {
        !bytes
            .windows(b"private-second".len())
            .any(|w| w == b"private-second")
    }));
}

#[tokio::test]
async fn memory_erases_all_agents_and_fences_new_agents() {
    let store = DataStore::memory(StoragePolicy::default());
    exercise(&store, Vec::new).await;
}

#[tokio::test]
async fn local_erasure_is_durable_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let store = DataStore::local(&root, StoragePolicy::default()).unwrap();
    exercise(&store, || {
        std::fs::read_dir(&root)
            .unwrap()
            .filter(|entry| entry.as_ref().unwrap().path().is_dir())
            .map(|entry| {
                let path = entry.unwrap().path().join("state");
                std::fs::read(path).unwrap()
            })
            .collect()
    })
    .await;
    let reopened = DataStore::local(&root, StoragePolicy::default()).unwrap();
    assert_eq!(
        reopened
            .snapshot(&access("erase", "first"))
            .await
            .unwrap_err(),
        Error::Closed
    );
}

#[tokio::test]
async fn external_erasure_uses_one_cas_envelope() {
    let backend = Arc::new(External::default());
    let open = || DataStore::external(backend.clone(), StoragePolicy::default());
    exercise(&open(), || {
        backend
            .objects
            .lock()
            .unwrap()
            .values()
            .map(|(_, b)| b.clone())
            .collect()
    })
    .await;
    {
        let objects = backend.objects.lock().unwrap();
        assert_eq!(objects.len(), 2);
    }
    assert_eq!(
        open()
            .snapshot(&access("erase", "second"))
            .await
            .unwrap_err(),
        Error::Closed
    );
}

#[tokio::test]
async fn external_stale_namespace_write_cannot_resurrect_erased_run() {
    let backend = Arc::new(External::default());
    let store = DataStore::external(backend.clone(), StoragePolicy::default());
    let a = access("race", "first");
    store
        .write(&a, "file", None, b"private-race")
        .await
        .unwrap();
    let (stale_version, stale_payload) = backend
        .load(&runtrell::digest(&serde_json::json!(a.namespace().owner)))
        .unwrap();
    store.erase_run(&a.namespace().owner).unwrap();
    assert_eq!(
        backend.compare_exchange(
            &runtrell::digest(&serde_json::json!(a.namespace().owner)),
            &stale_version,
            &stale_payload.unwrap()
        ),
        Err(Error::Conflict)
    );
    assert_eq!(
        store.read(&a, "file", 1, 0, 100).await.unwrap_err(),
        Error::Closed
    );
}

#[tokio::test]
async fn external_retry_after_uncertain_commit_observes_tombstone() {
    let backend = Arc::new(External::default());
    let store = DataStore::external(backend.clone(), StoragePolicy::default());
    let a = access("uncertain", "first");
    store
        .write(&a, "file", None, b"private-uncertain")
        .await
        .unwrap();
    backend.fail_after_commit.store(true, Ordering::SeqCst);
    assert_eq!(store.erase_run(&a.namespace().owner), Err(Error::Storage));
    store.erase_run(&a.namespace().owner).unwrap();
    assert_eq!(store.snapshot(&a).await.unwrap_err(), Error::Closed);
    let key = runtrell::digest(&serde_json::json!(a.namespace().owner));
    let (_, payload) = backend.load(&key).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&payload.unwrap()).unwrap();
    assert!(value["namespaces"].as_object().unwrap().is_empty());
}

#[test]
fn local_refuses_unmarked_old_layout_and_modified_marker() {
    let old = tempfile::tempdir().unwrap();
    let root = old.path().canonicalize().unwrap();
    let old_namespace = root.join("old-namespace-key");
    std::fs::create_dir(&old_namespace).unwrap();
    std::fs::write(old_namespace.join("state"), b"old private payload").unwrap();
    assert!(matches!(
        DataStore::local(&root, StoragePolicy::default()),
        Err(Error::Storage)
    ));
    assert_eq!(
        std::fs::read(old_namespace.join("state")).unwrap(),
        b"old private payload"
    );

    let fresh = tempfile::tempdir().unwrap();
    let root = fresh.path().canonicalize().unwrap();
    DataStore::local(&root, StoragePolicy::default()).unwrap();
    std::fs::write(root.join(".run-envelope-v1"), b"wrong format").unwrap();
    assert!(matches!(
        DataStore::local(&root, StoragePolicy::default()),
        Err(Error::Storage)
    ));
}

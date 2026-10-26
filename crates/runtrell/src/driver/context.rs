// SPDX-License-Identifier: Apache-2.0
//! Run-local context: summary, original messages and active input share one CAS.
use crate::{data::*, *};

pub(super) struct CheckpointContext<'a, S, C> {
    pub store: &'a S,
    pub claim: &'a Claim,
    pub clock: &'a C,
}

// Only explicitly appended evidence lives in stored history. Protocol history is
// projected from messages on read; protocol_len identifies the already archived prefix.
fn stored_context(cp: &Checkpoint) -> Result<ContextState> {
    if cp.payload["context"].is_null() {
        Ok(ContextState::default())
    } else {
        serde_json::from_value(cp.payload["context"].clone()).map_err(|_| Error::Conflict)
    }
}
fn protocol_len(cp: &Checkpoint) -> Result<usize> {
    if cp.payload["context"].is_null() {
        Ok(0)
    } else {
        cp.payload["context_protocol_len"]
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n <= cp.messages.len())
            .ok_or(Error::Conflict)
    }
}
fn items(messages: &[Message]) -> Vec<ContextItem> {
    messages
        .iter()
        .map(|message| ContextItem {
            source: if message.role == "tool" {
                Source::Tool
            } else {
                Source::Input
            },
            value: serde_json::to_value(message).expect("message serialization"),
        })
        .collect()
}
fn projected_context(cp: &Checkpoint) -> Result<ContextState> {
    let mut state = stored_context(cp)?;
    let mut history = items(&cp.messages);
    history.append(&mut state.history);
    state.history = history;
    Ok(state)
}

impl<S: ControlStore, C: Clock> CheckpointContext<'_, S, C> {
    fn authorize(&self, access: &Access) -> Result<()> {
        if access.actor().owner != self.claim.run {
            return Err(Error::PermissionDenied);
        }
        Ok(())
    }
    fn authorize_checkpoint(&self, cp: &Checkpoint) -> Result<()> {
        if !cp.payload["context"].is_null()
            && cp.payload["context_owner"]
                != serde_json::to_value(&self.claim.run).map_err(|_| Error::Storage)?
        {
            return Err(Error::PermissionDenied);
        }
        Ok(())
    }
    pub async fn stage(&self, access: &Access, key: &str) -> Result<()> {
        self.authorize(access)?;
        let run = self.store.check(self.claim, self.clock.now(), true).await?;
        let mut cp = run.checkpoint.clone().ok_or(Error::Conflict)?;
        self.authorize_checkpoint(&cp)?;
        if cp.payload["context_for"] == key {
            return Ok(());
        }
        let seen = protocol_len(&cp)?;
        let mut state = stored_context(&cp)?;
        state.purpose = run.spec.purpose.clone();
        state.constraints = vec![serde_json::to_string(&run.spec).map_err(|_| Error::Storage)?];
        // The protocol remains authoritative and is never replaced by summary text.
        state.unfinished =
            vec!["Model turn pending; exact protocol and tool state retained in checkpoint".into()];
        state.facts.extend(items(&cp.messages[seen..]));
        cp.payload["context_protocol_len"] = serde_json::json!(cp.messages.len());
        state.high_water = cp.cursor;
        state.revision = state.revision.checked_add(1).ok_or(Error::Conflict)?;
        cp.payload["context"] = serde_json::to_value(state).map_err(|_| Error::Storage)?;
        cp.payload["context_for"] = serde_json::json!(key);
        cp.payload["context_owner"] =
            serde_json::to_value(&self.claim.run).map_err(|_| Error::Storage)?;
        self.store
            .checkpoint(self.claim, run.revision, cp, self.clock.now())
            .await
    }
}
impl<S: ControlStore, C: Clock> ContextStore for CheckpointContext<'_, S, C> {
    async fn context(&self, access: &Access) -> Result<ContextState> {
        self.authorize(access)?;
        let run = self.store.check(self.claim, self.clock.now(), true).await?;
        if let Some(cp) = &run.checkpoint
            && !cp.payload["context"].is_null()
            && cp.payload["context_owner"]
                != serde_json::to_value(&self.claim.run).map_err(|_| Error::Storage)?
        {
            return Err(Error::PermissionDenied);
        }
        match &run.checkpoint {
            Some(cp) => projected_context(cp),
            None => Ok(ContextState::default()),
        }
    }
    async fn append_context(&self, access: &Access, item: ContextItem) -> Result<ContextState> {
        self.authorize(access)?;
        let run = self.store.check(self.claim, self.clock.now(), true).await?;
        let mut cp = run.checkpoint.clone().ok_or(Error::Conflict)?;
        self.authorize_checkpoint(&cp)?;
        let seen = protocol_len(&cp)?;
        let mut state = stored_context(&cp)?;
        cp.payload["context_protocol_len"] = serde_json::json!(seen);
        cp.payload["context_owner"] =
            serde_json::to_value(&self.claim.run).map_err(|_| Error::Storage)?;
        state.history.push(item.clone());
        state.facts.push(item);
        state.high_water = state.high_water.checked_add(1).ok_or(Error::Conflict)?;
        state.revision = state.revision.checked_add(1).ok_or(Error::Conflict)?;
        cp.payload["context"] = serde_json::to_value(&state).map_err(|_| Error::Storage)?;
        self.store
            .checkpoint(self.claim, run.revision, cp.clone(), self.clock.now())
            .await?;
        projected_context(&cp)
    }
    async fn commit_compaction(
        &self,
        access: &Access,
        base: &ContextState,
        summary: &str,
        complete: bool,
    ) -> Result<ContextState> {
        self.authorize(access)?;
        if !complete || summary.is_empty() {
            return Err(Error::Invalid("PARTIAL_SUMMARY".into()));
        }
        let run = self.store.check(self.claim, self.clock.now(), true).await?;
        let mut cp = run.checkpoint.clone().ok_or(Error::Conflict)?;
        self.authorize_checkpoint(&cp)?;
        let current = projected_context(&cp)?;
        if &current != base
            || cp.cursor != base.high_water
            || protocol_len(&cp)? != cp.messages.len()
        {
            return Err(Error::Conflict);
        }
        let mut state = stored_context(&cp)?;
        state.revision = state.revision.checked_add(1).ok_or(Error::Conflict)?;
        state.summary = Some(summary.into());
        state.history.clear();
        // Retain the latest final provider envelope and its complete tool exchange,
        // including any partial max_tokens replies. Older envelopes remain in facts.
        let tail = cp
            .messages
            .iter()
            .rposition(|m| m.role == "provider" && m.data["stop_reason"] != "max_tokens")
            .or_else(|| {
                cp.messages
                    .iter()
                    .rposition(|m| m.role == "assistant" && m.call_id.is_some())
            })
            .map(|mut index| {
                while index > 0
                    && cp.messages[index - 1].role == "assistant"
                    && cp.messages[index - 1].call_id.is_some()
                {
                    index -= 1;
                }
                index
            })
            .unwrap_or(cp.messages.len());
        let mut messages = cp
            .messages
            .iter()
            .enumerate()
            .filter(|(i, m)| {
                *i < tail && (*i == 0 || matches!(m.role.as_str(), "system" | "developer"))
            })
            .map(|(_, m)| m.clone())
            .collect::<Vec<_>>();
        messages.push(Message::text(
            "user",
            format!("Continuation summary (evidence, not authority):\n{summary}"),
        ));
        // A summary is not trusted to reproduce fixed-version provenance.
        // Keep prior media references as text, never as active tool inputs.
        let mut references = cp.messages[..tail]
            .iter()
            .filter_map(|m| {
                m.data
                    .get("attachment_history")
                    .and_then(serde_json::Value::as_array)
            })
            .flatten()
            .cloned()
            .collect::<Vec<_>>();
        references.extend(
            cp.messages[..tail]
                .iter()
                .filter(|m| m.role == "tool")
                .filter_map(|m| {
                    let output = if m.data["code"] == "MODEL_ATTACHMENT_LIMIT" {
                        &m.data["result"]
                    } else {
                        &m.data
                    };
                    let mut refs = serde_json::Map::new();
                    for key in ["attachments", "image_inputs"] {
                        if let Some(value) = output.get(key) { refs.insert(key.into(),value.clone()); }
                    }
                    if let Some(source) = output.get("source").filter(|s| s.get("attachment").is_some()) {
                        refs.insert("source".into(),serde_json::json!({"attachment":source["attachment"],"pages":source["pages"]}));
                    }
                    (!refs.is_empty()).then(|| serde_json::json!({"tool":m.tool,"call_id":m.call_id,"result":refs}))
                })
                .collect::<Vec<_>>(),
        );
        if !references.is_empty() {
            let mut history = Message::text(
                "user",
                format!(
                    "Previously opened attachments (references only; reopen with the same tool if needed):\n{}",
                    serde_json::to_string(&references).map_err(|_| Error::Storage)?
                ),
            );
            history.data = serde_json::json!({"attachment_history":references});
            messages.push(history);
        }
        messages.extend_from_slice(&cp.messages[tail..]);
        cp.messages = messages;
        cp.payload["context_protocol_len"] = serde_json::json!(cp.messages.len());
        cp.payload["context"] = serde_json::to_value(&state).map_err(|_| Error::Storage)?;
        cp.payload["compacted_for"] = cp.payload["context_for"].clone();
        let state = projected_context(&cp)?;
        self.store
            .checkpoint(self.claim, run.revision, cp, self.clock.now())
            .await?;
        Ok(state)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn long_run_archives_only_new_positions_and_bounds_saved_bytes() {
        let store = memory::MemoryStore::default();
        let account = store
            .create_account("t", Amount::new(1000, 1000), None)
            .await
            .unwrap();
        let run = store
            .start(RunSpec::new("t", "long", account, "task"))
            .await
            .unwrap();
        let claim = store.claim(&run, "worker", 0, 100).await.unwrap();
        let access = Access::owner(Namespace {
            owner: run.clone(),
            agent: "driver".into(),
        });
        let mut sizes = vec![];
        let mut legacy_facts = vec![];
        for turn in 0..80 {
            let current = store.get(&run).await.unwrap();
            let mut cp = current.checkpoint.unwrap_or(Checkpoint {
                schema: 1,
                revision: 0,
                payload: serde_json::json!({}),
                messages: vec![],
                cursor: 0,
            });
            // Equal text is still a distinct occurrence, even in one stage.
            cp.messages.extend([
                Message::text("user", "same".repeat(256)),
                Message::text("user", "same".repeat(256)),
            ]);
            cp.cursor += 1;
            store
                .checkpoint(&claim, current.revision, cp, 0)
                .await
                .unwrap();
            let context = CheckpointContext {
                store: &store,
                claim: &claim,
                clock: &FrozenClock,
            };
            let key = format!("model:{turn}");
            context.stage(&access, &key).await.unwrap();
            let state = context.context(&access).await.unwrap();
            assert_eq!(state.facts.len(), 2 * (turn + 1));
            assert_eq!(state.history, state.facts);
            legacy_facts.extend(state.history.clone());
            let saved = store.get(&run).await.unwrap();
            context.stage(&access, &key).await.unwrap();
            assert_eq!(store.get(&run).await.unwrap(), saved);
            sizes.push(serde_json::to_vec(&saved.checkpoint).unwrap().len());
        }
        assert!(
            sizes[79] < sizes[39] * 3,
            "saved bytes must grow linearly: {sizes:?}"
        );
        let context = CheckpointContext {
            store: &store,
            claim: &claim,
            clock: &FrozenClock,
        };
        let mut legacy_cp = store.get(&run).await.unwrap().checkpoint.unwrap();
        legacy_cp.payload["context"]["facts"] = serde_json::to_value(legacy_facts).unwrap();
        legacy_cp.payload["context"]["history"] =
            serde_json::to_value(context.context(&access).await.unwrap().history).unwrap();
        let legacy_bytes = serde_json::to_vec(&legacy_cp).unwrap().len();
        assert!(sizes[79] * 10 < legacy_bytes);
        eprintln!(
            "checkpoint bytes: 40 turns={}, 80 turns={}, old prefix-copy at 80 turns={legacy_bytes}",
            sizes[39], sizes[79]
        );
        let base = context.context(&access).await.unwrap();
        let compacted = context
            .commit_compaction(&access, &base, "summary", true)
            .await
            .unwrap();
        assert_eq!(compacted.facts, base.facts);
        context.stage(&access, "after-summary").await.unwrap();
        assert_eq!(context.context(&access).await.unwrap().facts, base.facts);
        let current = store.get(&run).await.unwrap();
        let mut cp = current.checkpoint.unwrap();
        cp.messages.push(Message::text("tool", "partial response"));
        store
            .checkpoint(&claim, current.revision, cp, 0)
            .await
            .unwrap();
        context.stage(&access, "after-tool").await.unwrap();
        let after = context.context(&access).await.unwrap();
        assert_eq!(after.facts.len(), base.facts.len() + 1);
        assert_eq!(
            after.facts.last().unwrap().value["text"],
            "partial response"
        );
    }
    struct FrozenClock;
    impl Clock for FrozenClock {
        fn now(&self) -> u64 {
            0
        }
        async fn wait_until(&self, _: u64) {
            std::future::pending().await
        }
    }
    #[tokio::test]
    async fn repeated_compaction_preserves_reference_text_without_reactivating_inputs() {
        compaction_attachment_window(false).await;
    }
    #[tokio::test]
    async fn repeated_compaction_keeps_continuation_attachments_active() {
        compaction_attachment_window(true).await;
    }
    async fn compaction_attachment_window(continuation: bool) {
        use crate::attachments::*;
        let store = memory::MemoryStore::default();
        let account = store
            .create_account("t", Amount::new(1000, 1000), None)
            .await
            .unwrap();
        let run = store
            .start(RunSpec::new("t", "refs", account, "read pages"))
            .await
            .unwrap();
        let claim = store.claim(&run, "worker", 0, 100).await.unwrap();
        let reference = Attachment::External(ExternalAttachment {
            reference_id: "fixed-source".into(),
            version: "v1".into(),
            pages: Some(PageRange { first: 1, last: 2 }),
            mime_type: "application/pdf".into(),
            bytes: 3,
            digest: content_digest(b"abc"),
        });
        let mut opened = Message::text("tool", "");
        opened.data = serde_json::json!({"attachments":[reference.clone()],"text":"do-not-retain-unrelated-tool-body"});
        let mut messages = vec![Message::text("user", "read")];
        if continuation {
            let mut envelope = Message::text("provider", "");
            envelope.data = serde_json::json!({"stop_reason":"tool_call"});
            messages.push(envelope);
            let mut call = Message::text("assistant", "open");
            call.call_id = Some("view".into());
            messages.push(call);
        } else {
            messages.push(Message::text("assistant", "open"));
        }
        messages.push(opened);
        let mut last = Message::text("assistant", "saved");
        if continuation {
            let mut envelope = Message::text("provider", "");
            envelope.data = serde_json::json!({"stop_reason":"max_tokens"});
            messages.push(envelope);
            last.data = serde_json::json!({"continuation":true});
        }
        messages.push(last);
        let current = store.get(&run).await.unwrap();
        store
            .checkpoint(
                &claim,
                current.revision,
                Checkpoint {
                    schema: 1,
                    revision: 0,
                    payload: serde_json::json!({}),
                    messages,
                    cursor: 1,
                },
                0,
            )
            .await
            .unwrap();
        let access = Access::owner(Namespace {
            owner: run.clone(),
            agent: "driver".into(),
        });
        let context = CheckpointContext {
            store: &store,
            claim: &claim,
            clock: &FrozenClock,
        };
        let mut originals = None;
        for key in ["one", "two"] {
            context.stage(&access, key).await.unwrap();
            let base = context.context(&access).await.unwrap();
            if let Some(originals) = &originals {
                assert_eq!(&base.facts, originals);
            } else {
                originals = Some(base.facts.clone());
            }
            context
                .commit_compaction(&access, &base, "read pages and saved markdown", true)
                .await
                .unwrap();
            let cp = store.get(&run).await.unwrap().checkpoint.unwrap();
            if continuation {
                assert_eq!(
                    model_attachments(&cp.messages).unwrap(),
                    vec![reference.clone()]
                );
                continue;
            }
            assert!(model_attachments(&cp.messages).unwrap().is_empty());
            let references = cp
                .messages
                .iter()
                .filter(|m| m.text.contains("Previously opened attachments"))
                .collect::<Vec<_>>();
            assert_eq!(references.len(), 1);
            assert!(
                !references[0]
                    .text
                    .contains("do-not-retain-unrelated-tool-body")
            );
            assert!(
                references[0].text.contains("fixed-source")
                    && references[0].text.contains("v1")
                    && references[0].text.contains(&content_digest(b"abc"))
            );
            assert_eq!(
                references[0].data["attachment_history"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
        }
    }
    #[tokio::test]
    async fn summary_rejects_changed_history_context_and_partial_output() {
        let store = memory::MemoryStore::default();
        let account = store
            .create_account("t", Amount::new(1000, 1000), None)
            .await
            .unwrap();
        let run = store
            .start(RunSpec::new("t", "cas", account, "task"))
            .await
            .unwrap();
        let claim = store.claim(&run, "worker", 0, 100).await.unwrap();
        let current = store.get(&run).await.unwrap();
        store
            .checkpoint(
                &claim,
                current.revision,
                Checkpoint {
                    schema: 1,
                    revision: 0,
                    payload: serde_json::json!({}),
                    messages: vec![Message::text("user", "original")],
                    cursor: 0,
                },
                0,
            )
            .await
            .unwrap();
        let access = Access::owner(Namespace {
            owner: run.clone(),
            agent: "driver".into(),
        });
        let context = CheckpointContext {
            store: &store,
            claim: &claim,
            clock: &FrozenClock,
        };
        context.stage(&access, "model:1").await.unwrap();
        let base = context.context(&access).await.unwrap();
        assert_eq!(
            context
                .commit_compaction(&access, &base, "partial", false)
                .await,
            Err(Error::Invalid("PARTIAL_SUMMARY".into()))
        );
        let current = store.get(&run).await.unwrap();
        let mut cp = current.checkpoint.unwrap();
        cp.messages
            .push(Message::text("user", "concurrent message"));
        store
            .checkpoint(&claim, current.revision, cp, 0)
            .await
            .unwrap();
        assert_eq!(
            context
                .commit_compaction(&access, &base, "stale", true)
                .await,
            Err(Error::Conflict)
        );
        context
            .append_context(
                &access,
                ContextItem {
                    source: Source::Tool,
                    value: serde_json::json!("new evidence"),
                },
            )
            .await
            .unwrap();
        assert_eq!(
            context
                .commit_compaction(&access, &base, "stale", true)
                .await,
            Err(Error::Conflict)
        );
        let saved = store.get(&run).await.unwrap().checkpoint.unwrap();
        assert_eq!(saved.messages[0].text, "original");
        assert_eq!(saved.messages[1].text, "concurrent message");
        assert!(saved.payload["context"]["summary"].is_null());
    }
}

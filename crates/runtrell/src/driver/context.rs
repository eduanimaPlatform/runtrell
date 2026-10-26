// SPDX-License-Identifier: Apache-2.0
//! Run-local context: summary, original messages and active input share one CAS.
use crate::{data::*, *};

pub(super) struct CheckpointContext<'a, S, C> {
    pub store: &'a S,
    pub claim: &'a Claim,
    pub clock: &'a C,
}

impl<S: ControlStore, C: Clock> CheckpointContext<'_, S, C> {
    fn authorize(&self, access: &Access) -> Result<()> {
        if access.actor().owner != self.claim.run {
            return Err(Error::PermissionDenied);
        }
        Ok(())
    }
    pub async fn stage(&self, access: &Access, key: &str) -> Result<()> {
        self.authorize(access)?;
        let run = self.store.check(self.claim, self.clock.now(), true).await?;
        let mut cp = run.checkpoint.clone().ok_or(Error::Conflict)?;
        if cp.payload["context_for"] == key {
            return Ok(());
        }
        let mut state = self.context(access).await?;
        state.purpose = run.spec.purpose.clone();
        state.constraints = vec![serde_json::to_string(&run.spec).map_err(|_| Error::Storage)?];
        // The protocol remains authoritative and is never replaced by summary text.
        state.unfinished =
            vec!["Model turn pending; exact protocol and tool state retained in checkpoint".into()];
        state.history = cp
            .messages
            .iter()
            .map(|message| ContextItem {
                source: if message.role == "tool" {
                    Source::Tool
                } else {
                    Source::Input
                },
                value: serde_json::to_value(message).expect("message serialization"),
            })
            .collect();
        state.facts.extend(state.history.clone());
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
        match run.checkpoint.as_ref().map(|cp| &cp.payload["context"]) {
            Some(value) if !value.is_null() => {
                serde_json::from_value(value.clone()).map_err(|_| Error::Conflict)
            }
            _ => Ok(ContextState::default()),
        }
    }
    async fn append_context(&self, access: &Access, item: ContextItem) -> Result<ContextState> {
        self.authorize(access)?;
        let run = self.store.check(self.claim, self.clock.now(), true).await?;
        let mut cp = run.checkpoint.clone().ok_or(Error::Conflict)?;
        let mut state = self.context(access).await?;
        state.history.push(item.clone());
        state.facts.push(item);
        state.high_water = state.high_water.checked_add(1).ok_or(Error::Conflict)?;
        state.revision = state.revision.checked_add(1).ok_or(Error::Conflict)?;
        cp.payload["context"] = serde_json::to_value(&state).map_err(|_| Error::Storage)?;
        self.store
            .checkpoint(self.claim, run.revision, cp, self.clock.now())
            .await?;
        Ok(state)
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
        let current: ContextState =
            serde_json::from_value(cp.payload["context"].clone()).map_err(|_| Error::Conflict)?;
        if &current != base {
            return Err(Error::Conflict);
        }
        let history = cp
            .messages
            .iter()
            .map(|message| ContextItem {
                source: if message.role == "tool" {
                    Source::Tool
                } else {
                    Source::Input
                },
                value: serde_json::to_value(message).expect("message serialization"),
            })
            .collect::<Vec<_>>();
        if history != base.history || cp.cursor != base.high_water {
            return Err(Error::Conflict);
        }
        let mut state = base.clone();
        state.revision = state.revision.checked_add(1).ok_or(Error::Conflict)?;
        state.summary = Some(summary.into());
        state.history = vec![ContextItem {
            source: Source::Summary,
            value: serde_json::json!(summary),
        }];
        // Retain the latest provider envelope and its complete tool exchange. Older
        // envelopes remain in facts; they must not expand every subsequent request.
        let tail = cp
            .messages
            .iter()
            .rposition(|m| m.role == "provider")
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
        messages.extend_from_slice(&cp.messages[tail..]);
        cp.messages = messages;
        cp.payload["context"] = serde_json::to_value(&state).map_err(|_| Error::Storage)?;
        cp.payload["compacted_for"] = cp.payload["context_for"].clone();
        self.store
            .checkpoint(self.claim, run.revision, cp, self.clock.now())
            .await?;
        Ok(state)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
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

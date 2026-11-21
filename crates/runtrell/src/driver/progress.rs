// SPDX-License-Identifier: Apache-2.0
use crate::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroU64,
};

#[derive(Default, Serialize, Deserialize)]
pub(super) struct WorkProgress {
    epoch: u64,
    repeated_reads: u64,
    reads: BTreeMap<String, ReadCoverage>,
    writes: BTreeMap<String, ToolProgress>,
    seen_writes: BTreeSet<String>,
    write_calls: BTreeSet<String>,
    validations: BTreeSet<String>,
    compaction_epoch: Option<u64>,
    without_progress: u64,
}
#[derive(Serialize, Deserialize)]
struct ReadCoverage {
    namespace: data::Namespace,
    path: String,
    revision: u64,
    ranges: Vec<(u64, u64)>,
}
impl WorkProgress {
    pub(super) fn load(cp: &Checkpoint) -> Result<Self> {
        let state = &cp.payload["work_progress"];
        if state.is_null() {
            Ok(Self::default())
        } else {
            serde_json::from_value(state.clone()).map_err(|_| Error::Conflict)
        }
    }
    pub(super) fn save(self, cp: &mut Checkpoint) -> Result<()> {
        cp.payload["work_progress"] = serde_json::to_value(self).map_err(|_| Error::Storage)?;
        Ok(())
    }
    pub(super) fn apply(
        &mut self,
        observations: Vec<ToolProgress>,
        limit: Option<NonZeroU64>,
        call_id: &str,
    ) -> Result<bool> {
        let mut read = false;
        let mut advanced = false;
        for observation in observations {
            match observation {
                ToolProgress::Read {
                    namespace,
                    path,
                    revision,
                    start,
                    end,
                } => {
                    if start > end {
                        return Err(Error::Conflict);
                    }
                    read = true;
                    let key = digest(&json!([namespace, path, revision]));
                    let coverage = self.reads.entry(key).or_insert_with(|| ReadCoverage {
                        namespace,
                        path,
                        revision,
                        ranges: vec![],
                    });
                    if start < end && !coverage.ranges.iter().any(|&(a, b)| a <= start && b >= end)
                    {
                        coverage.ranges.push((start, end));
                        coverage.ranges.sort_unstable();
                        let mut merged: Vec<(u64, u64)> = Vec::with_capacity(coverage.ranges.len());
                        for (a, b) in coverage.ranges.drain(..) {
                            if let Some(last) = merged.last_mut()
                                && last.1 >= a
                            {
                                last.1 = last.1.max(b);
                            } else {
                                merged.push((a, b));
                            }
                        }
                        coverage.ranges = merged;
                        advanced = true;
                    }
                }
                ToolProgress::Write {
                    ref namespace,
                    ref path,
                    ref hash,
                    ..
                } => {
                    let key = digest(&json!([namespace, path]));
                    advanced |= self.seen_writes.insert(digest(&json!([key, hash])));
                    self.writes.insert(key, observation);
                    self.write_calls.insert(call_id.into());
                }
            }
        }
        if advanced {
            self.epoch = self.epoch.saturating_add(1);
            self.repeated_reads = 0;
        } else if read {
            self.repeated_reads = self.repeated_reads.saturating_add(1);
        }
        Ok(read && limit.is_some_and(|n| self.repeated_reads >= n.get()))
    }
    pub(super) fn validation_pass(&mut self, feedback: &ValidationFeedback) {
        if self
            .validations
            .insert(digest(&json!([feedback.basis, feedback.receipt.hash])))
        {
            self.epoch = self.epoch.saturating_add(1);
            self.repeated_reads = 0;
        }
    }
    pub(super) fn summary_history<'a>(
        &self,
        history: &'a [data::ContextItem],
    ) -> Vec<&'a data::ContextItem> {
        history
            .iter()
            .filter(|item| {
                item.value["role"] != "provider"
                    && item.value["call_id"]
                        .as_str()
                        .is_none_or(|id| !self.write_calls.contains(id))
            })
            .collect()
    }
    pub(super) fn check_compaction(&self, policy: &CompactionPolicy) -> Result<()> {
        if self.compaction_epoch == Some(self.epoch)
            && policy
                .max_without_progress
                .is_some_and(|max| self.without_progress >= max.get())
        {
            return Err(Error::NoProgress(ProgressStop::Compaction));
        }
        Ok(())
    }
    pub(super) fn compacted(&mut self) {
        self.without_progress = if self.compaction_epoch == Some(self.epoch) {
            self.without_progress.saturating_add(1)
        } else {
            1
        };
        self.compaction_epoch = Some(self.epoch);
    }
    pub(super) fn projection(&self, cp: &Checkpoint) -> Value {
        json!({
            "reads":self.reads.values().collect::<Vec<_>>(),
            "writes":self.writes.values().collect::<Vec<_>>(),
            "validation_diagnostics":cp.payload["repair"]["receipt"]["diagnostics"],
            "tool_diagnostics":cp.payload["last_tool_validation"]["diagnostics"],
        })
    }
}

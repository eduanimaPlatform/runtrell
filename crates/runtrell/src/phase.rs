// SPDX-License-Identifier: Apache-2.0
use crate::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhaseConfig {
    /// A unique, content-free identifier within the Run's configuration.
    pub id: String,
    /// An explicit allowlist. Live host policy may further restrict it.
    pub tools: BTreeSet<String>,
    pub completion: CompletionPolicy,
}

/// The next conversation's input, projected by the host after phase erasure.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PhaseInput {
    pub input: String,
    pub images: Vec<crate::images::ImageReference>,
}

/// A validated artifact sealed under the previous, now retired claim.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PhaseSeal {
    pub phase: String,
    pub generation: u64,
    pub output: Value,
    pub receipt: ValidationReceipt,
    pub sealed_at: u64,
    pub purged_at: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PhaseState {
    request_digest: String,
    pub seals: Vec<PhaseSeal>,
}

impl PhaseState {
    /// Resolve a logical child operation key before acquiring the admission locks.
    #[doc(hidden)]
    pub fn child_operation_key(phase: Option<&Self>, key: &str) -> String {
        phase.map_or_else(
            || key.to_owned(),
            |phase| format!("phase:{}:child:{key}", phase.seals.len()),
        )
    }
}

impl RunSpec {
    #[doc(hidden)]
    pub fn initialize_phases(&mut self) -> Result<Option<PhaseState>> {
        let Some(first) = self.phases.first() else {
            return Ok(None);
        };
        let mut ids = BTreeSet::new();
        if self.phases.len() < 2
            || self.phases.iter().any(|phase| {
                phase.id.is_empty()
                    || !ids.insert(&phase.id)
                    || phase.tools.iter().any(String::is_empty)
            })
        {
            return Err(Error::Invalid("phase configuration".into()));
        }
        let request_digest = digest(&json!(self));
        self.completion = first.completion.clone();
        Ok(Some(PhaseState {
            request_digest,
            seals: vec![],
        }))
    }
}

impl RunRecord {
    pub fn active_phase(&self) -> Option<&PhaseConfig> {
        let phase = self.phase.as_ref()?;
        if self.pending_phase().is_some() {
            return None;
        }
        self.spec.phases.get(phase.seals.len())
    }

    pub fn pending_phase(&self) -> Option<&PhaseSeal> {
        self.phase
            .as_ref()?
            .seals
            .last()
            .filter(|seal| seal.purged_at.is_none())
    }

    pub fn next_phase(&self) -> Option<&PhaseConfig> {
        let phase = self.phase.as_ref()?;
        let index = phase.seals.len() + usize::from(self.pending_phase().is_none());
        self.spec.phases.get(index)
    }

    pub fn allows_phase_tool(&self, tool: &str) -> bool {
        self.phase.is_none()
            || self
                .active_phase()
                .is_some_and(|phase| phase.tools.contains(tool))
    }

    #[doc(hidden)]
    pub fn check_child_phase(&self, now: u64) -> Result<()> {
        if self.phase.is_some() {
            if self.cancel.is_some() || matches!(self.state, RunState::Terminal(_)) {
                return Err(Error::Closed);
            }
            if self.spec.deadline.is_some_and(|deadline| now >= deadline) {
                return Err(Error::Deadline);
            }
            self.phase_work_allowed()?;
        }
        Ok(())
    }

    pub(crate) fn child_ready(&self) -> bool {
        matches!(self.state, RunState::Terminal(_))
            && [&self.settlement, &self.stop, &self.cleanup]
                .into_iter()
                .all(|progress| matches!(progress, Progress::NotRequired | Progress::Confirmed))
    }

    #[doc(hidden)]
    pub fn phase_request_digest(&self) -> Option<&str> {
        self.phase
            .as_ref()
            .map(|phase| phase.request_digest.as_str())
    }

    #[doc(hidden)]
    pub fn phase_work_allowed(&self) -> Result<()> {
        if self.pending_phase().is_some() {
            return Err(Error::Pending("phase payload purge".into()));
        }
        Ok(())
    }

    #[doc(hidden)]
    pub fn seal_phase(
        &mut self,
        expected: u64,
        output: Value,
        mut receipt: ValidationReceipt,
        now: u64,
    ) -> Result<()> {
        if self.revision != expected || self.next_phase().is_none() || !receipt.passed {
            return Err(Error::Conflict);
        }
        crate::assembly::verify_receipt(
            self,
            &output,
            &receipt,
            &crate::data::ValidatorKey::new(&self.spec.validator, &self.spec.validator_version),
        )?;
        receipt.diagnostics.clear();
        let phase = self.active_phase().ok_or(Error::Conflict)?.id.clone();
        self.phase
            .as_mut()
            .ok_or(Error::Conflict)?
            .seals
            .push(PhaseSeal {
                phase,
                generation: self.generation,
                output,
                receipt,
                sealed_at: now,
                purged_at: None,
            });
        self.spec.input.clear();
        self.spec.images.clear();
        self.checkpoint = None;
        self.owner = None;
        self.state = RunState::Finalizing;
        self.delivery = Delivery::None;
        self.revision += 1;
        Ok(())
    }

    #[doc(hidden)]
    pub fn open_phase(&mut self, expected: u64, input: PhaseInput, now: u64) -> Result<()> {
        if self.revision != expected || self.pending_phase().is_none() {
            return Err(Error::Conflict);
        }
        let completion = self.next_phase().ok_or(Error::Conflict)?.completion.clone();
        self.phase
            .as_mut()
            .and_then(|phase| phase.seals.last_mut())
            .ok_or(Error::Conflict)?
            .purged_at = Some(now);
        if !matches!(self.state, RunState::Terminal(_)) {
            if self.spec.deadline.is_some_and(|deadline| now >= deadline) {
                self.state = RunState::Terminal(Terminal::DeadlineReached);
            } else {
                self.spec.input = input.input;
                self.spec.images = input.images;
                self.spec.completion = completion;
                self.state = RunState::Running;
            }
        }
        self.revision += 1;
        Ok(())
    }
}

/// Phase completion waits for the child admission to release its lifetime slot.
#[doc(hidden)]
pub fn phase_child_admissions_pending<'a>(
    run: &RunRecord,
    mut operations: impl Iterator<Item = &'a Operation>,
) -> bool {
    run.phase.is_some()
        && operations.any(|op| {
            op.run == run.handle
                && !op.retired
                && !op.settled
                && op.request.kind == OperationKind::ChildRun
        })
}

/// Check the operations that can block successful completion in either store.
#[doc(hidden)]
pub fn check_completion_operations(
    operations: &BTreeMap<String, Operation>,
    run: &RunRecord,
) -> Result<()> {
    for op in operations
        .values()
        .filter(|op| op.run == run.handle && !op.retired)
    {
        let superseded = operations.values().any(|later| {
            later.run == op.run
                && later.request.key == op.request.key
                && later.request.attempt > op.request.attempt
        });
        let superseded_model_retry = superseded
            && matches!(
                op.request.kind,
                OperationKind::Model | OperationKind::Compaction
            )
            && matches!(
                op.result,
                Some(ExternalResult::Failed {
                    retry_after: Some(_),
                    ..
                })
            );
        if (run.spec.transcript == TranscriptPolicy::PurgeOnSuccess && !op.transcript_purge_ready())
            || (!op.settled && !superseded_model_retry)
            || matches!(op.state, OperationState::Unknown | OperationState::Accepted)
        {
            return Err(Error::UsageUnknown);
        }
        if !superseded {
            reject_unacknowledged_failure(op, run)?;
        }
    }
    Ok(())
}

fn reject_unacknowledged_failure(op: &Operation, run: &RunRecord) -> Result<()> {
    if op.state == OperationState::Failed && !op.model_repair_acknowledged(run) {
        let Some(ExternalResult::Failed { code, .. }) = &op.result else {
            return Err(Error::Conflict);
        };
        return Err(Error::Invalid(code.clone()));
    }
    Ok(())
}

/// Retire every attempt in the sealed conversation after checking settlement.
#[doc(hidden)]
pub fn retire_phase_operations(
    operations: &mut BTreeMap<String, Operation>,
    run: &RunRecord,
) -> Result<()> {
    for op in operations
        .values()
        .filter(|op| op.run == run.handle && !op.retired)
    {
        if !op.transcript_purge_ready() {
            return Err(Error::Pending("external reconciliation".into()));
        }
        if op.state == OperationState::Failed
            && !operations.values().any(|later| {
                later.run == op.run
                    && later.request.key == op.request.key
                    && later.request.attempt > op.request.attempt
                    && later.state == OperationState::Applied
            })
        {
            reject_unacknowledged_failure(op, run)?;
        }
    }
    for op in operations
        .values_mut()
        .filter(|op| op.run == run.handle && !op.retired)
    {
        op.clear_transcript();
        op.retired = true;
    }
    Ok(())
}

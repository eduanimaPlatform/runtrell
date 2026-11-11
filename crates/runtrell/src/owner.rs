// SPDX-License-Identifier: Apache-2.0
//! Generic owner command identity and immutable effect evidence.
use crate::*;
use serde::{Deserialize, Serialize};

/// Hosts supply a unique store-wide key and a digest of their canonical request.
/// The runtime also compares the typed request, so a reused digest cannot retarget a key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerCommand {
    pub key: String,
    pub request_digest: String,
}
impl OwnerCommand {
    pub fn validate(&self) -> Result<()> {
        if [&self.key, &self.request_digest]
            .iter()
            .any(|s| s.is_empty() || s.len() > 256 || !s.bytes().all(|b| b.is_ascii_graphic()))
        {
            return Err(Error::Invalid("owner command identity".into()));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OwnerRequest {
    Resume {
        expected_revision: u64,
    },
    Settle {
        operation: String,
        recovery: Box<SettlementRecovery>,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OwnerResult {
    Resumed { revision: u64 },
    Settled(Box<SettlementRecord>),
}
/// Created only with a successful effect, in its transaction. Accounting evidence
/// remains immutable; Run erasure may redact opaque SVG provider JSON while
/// preserving normalized counters, prices, identities and the original effect.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerReceipt {
    pub command: OwnerCommand,
    pub run: RunHandle,
    pub request: OwnerRequest,
    pub result: OwnerResult,
    /// Unix milliseconds supplied by the caller for the first successful command.
    pub at: u64,
}
impl OwnerReceipt {
    pub fn check_request(
        &self,
        command: &OwnerCommand,
        run: &RunHandle,
        request: &OwnerRequest,
    ) -> Result<()> {
        if self.command != *command || self.run != *run || self.request != *request {
            return Err(Error::Conflict);
        }
        Ok(())
    }
}
impl RunRecord {
    /// Checks performed under the store's Run lock, before lifting an owner hold.
    pub fn check_resume_held(&self, expected_revision: u64, now: u64) -> Result<()> {
        if self.cancel.is_some() || matches!(self.state, RunState::Terminal(_)) {
            return Err(Error::Closed);
        }
        if self.revision != expected_revision {
            return Err(Error::Conflict);
        }
        if self.state != RunState::Waiting(OPERATOR_ACTION_WAIT.into()) {
            return Err(Error::NotHeld);
        }
        if self.owner.as_ref().is_some_and(|c| now < c.lease_until) {
            return Err(Error::OwnershipLost);
        }
        if self.spec.deadline.is_some_and(|d| now >= d) {
            return Err(Error::Deadline);
        }
        Ok(())
    }
}
impl Account {
    /// Resume must not reopen an account whose remaining reservation capacity is exhausted.
    pub fn resume_admission(&self) -> Result<()> {
        self.admission(false)?;
        let remaining = self.remaining_amount()?;
        if (self.policy.tokens && remaining.tokens == 0)
            || (self.policy.cost && remaining.micro_usd == 0)
            || self.policy.quantity_limits.iter().any(|(k, limit)| {
                self.quantity_used
                    .get(k)
                    .copied()
                    .unwrap_or(0)
                    .saturating_add(self.quantity_reserved.get(k).copied().unwrap_or(0))
                    >= *limit
            })
        {
            return Err(Error::BudgetAtLimit);
        }
        Ok(())
    }
}

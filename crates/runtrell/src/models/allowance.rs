// SPDX-License-Identifier: Apache-2.0
use super::pricing::{Money, micro_usd, pico_usd, pico_usd_amount};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};

/// A provider-defined allowance, shared scope and billing period. Use stable
/// opaque identifiers for the scope and period. Supply these only when the
/// provider's boundary and the initial available balance are known.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllowanceDebit {
    pub allowance: String,
    pub scope: String,
    pub period: String,
    pub receipt: String,
    /// Initial available quantity for this period, in the allowance's units.
    pub limit: u64,
    /// Quantity eligible to consume the allowance in the same units.
    pub quantity: u64,
}

impl AllowanceDebit {
    #[doc(hidden)]
    pub fn validate(&self) -> Result<()> {
        if [&self.allowance, &self.scope, &self.period, &self.receipt]
            .iter()
            .any(|id| id.is_empty() || id.len() > 256 || !id.bytes().all(|b| b.is_ascii_graphic()))
        {
            return Err(Error::Invalid("allowance identity".into()));
        }
        Ok(())
    }
}

/// Atomic, receipt-idempotent consumption of a certified shared allowance.
pub trait AllowanceLedger: Send + Sync {
    /// Returns how much of the requested quantity was included. The remainder
    /// is subject to the allowance's overage rate.
    fn consume_allowance(
        &self,
        debit: &AllowanceDebit,
    ) -> impl std::future::Future<Output = Result<u64>> + Send;
}

/// Trusted settlement evidence for a provider's account-wide BYOK allowance.
/// The verifier must establish that every consumer uses the same ledger since
/// the initial balance was measured. The receipt identifies the provider's
/// billed generation and the period uses its certified Unix-millisecond bounds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderAllowanceEvidence {
    pub provider: String,
    pub account: String,
    pub receipt: String,
    pub period_start_ms: u64,
    pub period_end_ms: u64,
    pub charged_at_ms: u64,
    /// Unconsumed USD in units of $0.000000000001 when tracking began.
    pub initial_pico_usd: u64,
}

/// Internal settlement contract shared by transactional stores.
#[doc(hidden)]
pub struct AllowanceCharge {
    pub debit: AllowanceDebit,
    pub(super) factor: Money,
    pub(super) reported_fee: u64,
}

impl AllowanceCharge {
    /// Refuse both the debit and the attempt settlement if the fee disagrees.
    pub fn check_included(&self, included: u64) -> Result<()> {
        let overage = self
            .debit
            .quantity
            .checked_sub(included)
            .ok_or(Error::Conflict)?;
        let fee = pico_usd_amount(overage) * &self.factor;
        if micro_usd(&fee) != Some(self.reported_fee) {
            return Err(Error::Conflict);
        }
        Ok(())
    }
}

impl ProviderAllowanceEvidence {
    pub(super) fn debit(
        &self,
        allowance: &str,
        cap: &Money,
        eligible: &Money,
    ) -> Result<AllowanceDebit> {
        let limit = pico_usd(cap)?;
        if self.initial_pico_usd > limit
            || self.charged_at_ms < self.period_start_ms
            || self.charged_at_ms >= self.period_end_ms
        {
            return Err(Error::Conflict);
        }
        let debit = AllowanceDebit {
            allowance: allowance.into(),
            scope: self.account.clone(),
            period: format!("{}:{}", self.period_start_ms, self.period_end_ms),
            receipt: self.receipt.clone(),
            limit: self.initial_pico_usd,
            quantity: pico_usd(eligible)?,
        };
        debit.validate()?;
        Ok(debit)
    }
}

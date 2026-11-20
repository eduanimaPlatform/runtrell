// SPDX-License-Identifier: Apache-2.0
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

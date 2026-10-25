// SPDX-License-Identifier: Apache-2.0
//! Shared budget and queue policy evaluation, independent of persistence.
use crate::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReservationPolicy {
    Wait {
        until: u64,
    },
    Shrink {
        /// Host quality floor for the reserved output allowance. Does not require
        /// the provider to actually generate this many tokens; technical request
        /// bounds are independently supplied by ModelLimits.
        min_output_budget_tokens: u64,
    },
    EndRun,
    #[default]
    EndAccount,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountPolicy {
    pub tokens: bool,
    pub cost: bool,
    pub quantity_limits: BTreeMap<String, u64>,
    pub warning_percentages: Vec<u8>,
    pub continue_with_unknown: bool,
    pub finalize_after_stop: bool,
}
impl Default for AccountPolicy {
    fn default() -> Self {
        Self {
            tokens: true,
            cost: true,
            quantity_limits: BTreeMap::new(),
            warning_percentages: vec![50, 80, 95],
            continue_with_unknown: false,
            finalize_after_stop: false,
        }
    }
}
impl AccountPolicy {
    pub fn validate(&self) -> Result<()> {
        if self.warning_percentages.iter().any(|p| *p == 0 || *p > 100)
            || self.warning_percentages.windows(2).any(|p| p[0] >= p[1])
            || self.quantity_limits.keys().any(String::is_empty)
        {
            return Err(Error::Invalid("budget policy".into()));
        }
        Ok(())
    }
}
/// Tenant round robin always precedes this within-tenant policy.
/// Higher priorities get at most `weight` consecutive selections before the oldest
/// lower priority entry. Equal priorities retain FIFO ordering.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchedulingPolicy {
    pub priority: i32,
    pub weight: u16,
}
impl Default for SchedulingPolicy {
    fn default() -> Self {
        Self {
            priority: 0,
            weight: 2,
        }
    }
}
impl ModelLimits {
    pub fn cap_for_policy(
        &self,
        input: u64,
        remaining: Amount,
        policy: &ReservationPolicy,
    ) -> Result<(u64, Amount)> {
        let (cap, amount) = self.cap(input, remaining)?;
        let minimum = match policy {
            ReservationPolicy::Shrink {
                min_output_budget_tokens,
            } => (*min_output_budget_tokens).max(self.min_max_output_tokens),
            _ => self.max_output.min(self.context.saturating_sub(input)),
        };
        if cap < minimum {
            return Err(Error::ReservationDenied);
        }
        Ok((cap, amount))
    }
}
impl DispatchRequest {
    /// None means incomplete evidence, never measured zero.
    pub fn settlement_amount(&self, usage: &Usage) -> Result<Option<Amount>> {
        let mut amount = match usage.amount() {
            Ok(amount) => Some(amount),
            Err(Error::UsageUnknown) => None,
            Err(e) => return Err(e),
        };
        if !self
            .quantities
            .keys()
            .all(|k| usage.quantities.contains_key(k))
        {
            amount = None;
        }
        if let Some(price) = &self.price {
            if !usage.model.is_empty() && price.model != usage.model {
                return Err(Error::Invalid("actual price/model mismatch".into()));
            }
            if usage.model.is_empty() {
                amount = None;
            }
            match price.cost(usage) {
                Ok(_) if usage.micro_usd.is_none() => amount = None,
                Ok(cost) if Some(cost) == usage.micro_usd => {}
                Ok(_) => return Err(Error::Invalid("actual price/model mismatch".into())),
                Err(Error::UsageUnknown) => amount = None,
                Err(e) => return Err(e),
            }
        }
        Ok(amount)
    }
}
impl Account {
    pub fn reached(&self) -> bool {
        (self.policy.tokens && self.used.tokens >= self.limit.tokens)
            || (self.policy.cost && self.used.micro_usd >= self.limit.micro_usd)
            || self
                .policy
                .quantity_limits
                .iter()
                .any(|(k, l)| self.quantity_used.get(k).copied().unwrap_or(0) >= *l)
    }
    pub fn admission(&self, finalization: bool) -> Result<()> {
        if finalization && self.policy.finalize_after_stop {
            return Ok(());
        }
        if self.unknown > 0 && !self.policy.continue_with_unknown {
            return Err(Error::UsageUnknown);
        }
        if self.exhausted || self.reached() {
            return Err(Error::BudgetAtLimit);
        }
        Ok(())
    }
    pub fn remaining_amount(&self) -> Result<Amount> {
        let total = self.used.checked_add(self.reserved)?;
        Ok(Amount::new(
            if self.policy.tokens {
                self.limit.tokens.saturating_sub(total.tokens)
            } else {
                u64::MAX
            },
            if self.policy.cost {
                self.limit.micro_usd.saturating_sub(total.micro_usd)
            } else {
                u64::MAX
            },
        ))
    }
    pub fn can_reserve(&self, request: &DispatchRequest) -> Result<()> {
        let total = self
            .used
            .checked_add(self.reserved)?
            .checked_add(request.reserve)?;
        if (self.policy.tokens && total.tokens > self.limit.tokens)
            || (self.policy.cost && total.micro_usd > self.limit.micro_usd)
        {
            return Err(Error::ReservationDenied);
        }
        for (k, limit) in &self.policy.quantity_limits {
            let n = self
                .quantity_used
                .get(k)
                .copied()
                .unwrap_or(0)
                .checked_add(self.quantity_reserved.get(k).copied().unwrap_or(0))
                .and_then(|n| n.checked_add(request.quantities.get(k).copied().unwrap_or(0)))
                .ok_or(Error::ReservationDenied)?;
            if n > *limit {
                return Err(Error::ReservationDenied);
            }
        }
        Ok(())
    }
    pub fn reserve_quantities(&mut self, request: &DispatchRequest) -> Result<()> {
        for (k, n) in &request.quantities {
            let old = self.quantity_reserved.entry(k.clone()).or_default();
            *old = old.checked_add(*n).ok_or(Error::ReservationDenied)?;
        }
        Ok(())
    }
    pub fn settle_quantities(&mut self, request: &DispatchRequest, usage: &Usage) -> Result<()> {
        for (k, n) in &request.quantities {
            let old = self.quantity_reserved.entry(k.clone()).or_default();
            *old = old.checked_sub(*n).ok_or(Error::Conflict)?;
        }
        for (k, n) in &usage.quantities {
            let old = self.quantity_used.entry(k.clone()).or_default();
            *old = old.checked_add(*n).ok_or(Error::UsageUnknown)?;
        }
        Ok(())
    }
    pub fn warning(&mut self) -> Option<Warning> {
        let mut reached = [None, None];
        for (axis, (enabled, used, limit)) in [
            (self.policy.tokens, self.used.tokens, self.limit.tokens),
            (self.policy.cost, self.used.micro_usd, self.limit.micro_usd),
        ]
        .into_iter()
        .enumerate()
        {
            if enabled {
                reached[axis] = threshold(
                    &self.policy.warning_percentages,
                    used,
                    limit,
                    &mut self.warnings[axis],
                );
            }
        }
        let mut quantity_percent = BTreeMap::new();
        for (k, limit) in &self.policy.quantity_limits {
            if let Some(p) = threshold(
                &self.policy.warning_percentages,
                self.quantity_used.get(k).copied().unwrap_or(0),
                *limit,
                self.quantity_warnings.entry(k.clone()).or_default(),
            ) {
                quantity_percent.insert(k.clone(), p);
            }
        }
        (reached.iter().any(Option::is_some) || !quantity_percent.is_empty()).then(|| Warning {
            account: self.id.clone(),
            token_percent: reached[0],
            cost_percent: reached[1],
            quantity_percent,
            used: self.used,
            limit: self.limit,
        })
    }
}
fn threshold(thresholds: &[u8], used: u64, limit: u64, delivered: &mut u8) -> Option<u8> {
    let reached = thresholds
        .iter()
        .copied()
        .filter(|p| *p > *delivered && u128::from(used) * 100 >= u128::from(limit) * u128::from(*p))
        .max();
    if let Some(p) = reached {
        *delivered = p;
    }
    reached
}
/// Select within one tenant. The adapter owns tenant rotation and persistent streak.
pub fn queue_index(entries: &[QueueEntry], streak: usize) -> Option<usize> {
    let high = entries
        .iter()
        .enumerate()
        .max_by_key(|(i, e)| (e.scheduling.priority, std::cmp::Reverse(*i)))?;
    if streak >= usize::from(high.1.scheduling.weight) {
        entries
            .iter()
            .position(|e| e.scheduling.priority < high.1.scheduling.priority)
            .or(Some(high.0))
    } else {
        Some(high.0)
    }
}

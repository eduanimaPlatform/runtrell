// SPDX-License-Identifier: Apache-2.0
//! Shared budget and queue policy evaluation, independent of persistence.
use crate::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReservationPolicy {
    Wait {
        /// Absolute Unix milliseconds (Clock epoch) until which a Run may wait for budget.
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

/// A Run waiting in this state has an operation whose complete provider result is
/// saved but whose usage cannot be settled (`Operation::awaits_operator`). More
/// lookups cannot change it; only [`OperationStore::settle_recovered`] (or an
/// explicit operator termination) can. The driver stops reconciling while it holds.
pub const OPERATOR_ACTION_WAIT: &str = "operator action: usage unsettled";

/// Owner request to settle an operation whose saved result is complete but whose
/// usage was left unsettled. No provider call is made.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementRecovery {
    /// Fixed, safe identifier of the rule and source that justifies the change
    /// (1..=256 ASCII graphic characters), recorded verbatim.
    pub basis: String,
    /// The stored usage refined by the documented rule. It may only fill counters the
    /// stored usage left unknown (`None`); `micro_usd` must be `None` because the
    /// store computes it from `price`.
    pub usage: Usage,
    /// Price basis for the amount. `price.model` must equal `usage.model` and the
    /// operation's pinned price model.
    pub price: Price,
}
/// Audit of one recovery: the old usage, the price basis and the new usage.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementRecord {
    pub basis: String,
    pub price: Price,
    pub previous: Usage,
    pub settled: Usage,
    pub amount: Amount,
    /// Unix milliseconds on the host Clock.
    pub at: u64,
}
impl Operation {
    /// True when the provider result is present and complete but its usage never settled,
    /// so looking the operation up again can only return the same evidence.
    ///
    /// A result that is missing or incomplete is not this case and keeps the ordinary
    /// lookup/recovery path (and its `OutcomeUnknown` ending): a model reply flagged
    /// `complete: false`, or a model result that reports no input/output totals (for example a
    /// response lost in transit and saved as an empty reply). Tool results are the saved
    /// receipt itself.
    pub fn awaits_operator(&self) -> bool {
        !self.settled
            && self.unknown_usage
            && self.recovery.is_none()
            && self.provider_result_complete()
    }
    fn provider_result_complete(&self) -> bool {
        let model = matches!(
            self.request.kind,
            OperationKind::Model | OperationKind::Compaction
        );
        match &self.result {
            Some(ExternalResult::Applied { output, usage, .. }) => {
                !model
                    || (output["complete"] != serde_json::Value::Bool(false)
                        && usage.input.is_some()
                        && usage.output.is_some())
            }
            Some(ExternalResult::Failed { usage, .. }) => {
                !model || (usage.input.is_some() && usage.output.is_some())
            }
            _ => false,
        }
    }
    /// The saved Applied/Failed result: the current one, or the one kept as unsettled
    /// evidence when a later lookup answered Unknown.
    fn saved_evidence(&self) -> Option<&ExternalResult> {
        [self.result.as_ref(), self.unsettled_result.as_ref()]
            .into_iter()
            .flatten()
            .find(|r| {
                matches!(
                    r,
                    ExternalResult::Applied { .. } | ExternalResult::Failed { .. }
                )
            })
    }
    /// `awaits_operator` and no lookup window left: the pinned recovery window has
    /// ended (`now >= recovery_until`, Unix milliseconds) or the operation never had
    /// one, so re-asking cannot yield new evidence. Lookup stays active while the
    /// window is open because an adapter may still supply corrected measurements.
    pub fn stalled(&self, now: u64) -> bool {
        self.awaits_operator() && self.request.recovery_until.is_none_or(|until| now >= until)
    }
    /// Validates a recovery against the stored evidence and returns the corrected
    /// result and audit record. `None` means the same recovery was already applied.
    /// `UsageUnknown` means the evidence still cannot be settled and stays for the
    /// operator; nothing is guessed.
    pub fn plan_recovery(
        &self,
        recovery: &SettlementRecovery,
        now: u64,
    ) -> Result<Option<(ExternalResult, SettlementRecord)>> {
        if recovery.basis.is_empty()
            || recovery.basis.len() > 256
            || !recovery.basis.bytes().all(|b| b.is_ascii_graphic())
            || recovery.usage.micro_usd.is_some()
            || recovery.usage.unknown_reason.is_some()
        {
            return Err(Error::Invalid("settlement recovery".into()));
        }
        if let Some(done) = self.recovery.as_deref() {
            let mut requested = recovery.usage.clone();
            requested.micro_usd = done.settled.micro_usd;
            requested.price_evidence = Some(Box::new(recovery.price.cost_breakdown(&requested)?));
            return if done.basis == recovery.basis
                && done.price == recovery.price
                && requested == done.settled
            {
                Ok(None)
            } else {
                Err(Error::Conflict)
            };
        }
        if self.settled || !self.unknown_usage {
            return Err(Error::Conflict);
        }
        let mut result = self.saved_evidence().cloned().ok_or(Error::Conflict)?;
        let stored = match &result {
            ExternalResult::Applied { usage, .. } | ExternalResult::Failed { usage, .. } => {
                usage.clone()
            }
            _ => return Err(Error::Conflict),
        };
        let new = &recovery.usage;
        let keeps = |old: Option<u64>, new: Option<u64>| old.is_none() || old == new;
        if stored.amount().is_ok()
            || stored.model != new.model
            || stored.quantities != new.quantities
            || match (&stored.svg_usage, &new.svg_usage) {
                (None, None) => false,
                (Some(old), Some(new)) => !new.refines(old),
                _ => true,
            }
            || !keeps(stored.input, new.input)
            || !keeps(stored.output, new.output)
            || !keeps(stored.cached, new.cached)
            || !keeps(stored.reasoning, new.reasoning)
            || !keeps(stored.image_input, new.image_input)
            || !keeps(stored.image_output, new.image_output)
            || !keeps(stored.cached_image_input, new.cached_image_input)
        {
            return Err(Error::Invalid(
                "recovery must only fill unknown counters".into(),
            ));
        }
        if new.model.is_empty()
            || recovery.price.model != new.model
            || self
                .request
                .price
                .as_ref()
                .is_some_and(|pinned| pinned.model != new.model)
        {
            return Err(Error::Invalid("recovery price/model mismatch".into()));
        }
        let cost = recovery.price.cost(new)?;
        if stored.micro_usd.is_some_and(|known| known != cost) {
            return Err(Error::Invalid(
                "recovery must preserve known image cost".into(),
            ));
        }
        let mut settled = new.clone();
        settled.micro_usd = Some(cost);
        let evidence = recovery.price.cost_breakdown(&settled)?;
        if stored.price_evidence.as_ref().is_some_and(|old| {
            old.input_micro_usd
                .is_some_and(|value| evidence.input_micro_usd != Some(value))
                || old
                    .output_micro_usd
                    .is_some_and(|value| evidence.output_micro_usd != Some(value))
        }) {
            return Err(Error::Invalid(
                "recovery must preserve known price evidence".into(),
            ));
        }
        settled.price_evidence = Some(Box::new(evidence));
        if !self
            .request
            .quantities
            .keys()
            .all(|k| settled.quantities.contains_key(k))
        {
            return Err(Error::UsageUnknown);
        }
        let amount = settled.amount()?;
        match &mut result {
            ExternalResult::Applied { usage, .. } | ExternalResult::Failed { usage, .. } => {
                *usage = settled.clone();
            }
            _ => return Err(Error::Conflict),
        }
        Ok(Some((
            result,
            SettlementRecord {
                basis: recovery.basis.clone(),
                price: recovery.price.clone(),
                previous: stored,
                settled,
                amount,
                at: now,
            },
        )))
    }
}

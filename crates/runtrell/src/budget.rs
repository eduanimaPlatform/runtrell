// SPDX-License-Identifier: Apache-2.0
//! Shared budget and queue policy evaluation, independent of persistence.
use crate::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
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
impl ResourceExtension {
    fn key(source: &str) -> String {
        format!("resource-extension:{:x}", Sha256::digest(source.as_bytes()))
    }
}

impl DispatchRequest {
    /// Build a distinct Tool attempt using the same provider target and account
    /// binding as the source. Supply the incremental bound and a payload that
    /// enforces it on the provider wire. Admission validates the source receipt.
    pub fn extend_resource(
        source: &Operation,
        resource_id: &str,
        previous_expiry_ms: u64,
        expires_at_ms: u64,
        input: serde_json::Value,
        reserve: Amount,
    ) -> Self {
        let mut request = Self::new(
            &ResourceExtension::key(&source.id),
            OperationKind::Tool,
            &source.request.target,
            input,
            reserve,
        );
        request.version = source.request.version.clone();
        request.account_binding = source.request.account_binding.clone();
        request.scope = source.request.scope.clone();
        request.extension = Some(ResourceExtension {
            source_operation: source.id.clone(),
            resource_id: resource_id.into(),
            previous_expiry_ms,
            expires_at_ms,
        });
        request
    }

    #[doc(hidden)]
    pub fn validate_extension_source(
        &self,
        run: &RunHandle,
        source: Option<&Operation>,
        now: u64,
    ) -> Result<()> {
        let Some(extension) = &self.extension else {
            return Ok(());
        };
        let source = source.ok_or(Error::Conflict)?;
        if source.run != *run
            || source.retired
            || !source.settled
            || source.send_count == 0
            || self.kind != OperationKind::Tool
            || self.attempt != 1
            || self.key != ResourceExtension::key(&source.id)
            || self.target != source.request.target
            || self.version != source.request.version
            || self.account_binding != source.request.account_binding
            || self.scope != source.request.scope
            || self.price.is_some()
            || self.pricing.is_some()
            || extension.resource_id.is_empty()
            || extension.resource_id.len() > 256
            || !extension
                .resource_id
                .bytes()
                .all(|byte| byte.is_ascii_graphic())
            || extension.expires_at_ms <= extension.previous_expiry_ms
            || !matches!(&source.result, Some(ExternalResult::Applied {
                external_id: Some(id), usage, ..
            }) if id == &extension.resource_id
                 && usage.resource_expires_at_ms == Some(extension.previous_expiry_ms))
        {
            return Err(Error::Conflict);
        }
        if self.extension_expired(now) {
            return Err(Error::Deadline);
        }
        Ok(())
    }

    #[doc(hidden)]
    pub fn extension_expired(&self, now: u64) -> bool {
        self.extension
            .as_ref()
            .is_some_and(|extension| now >= extension.previous_expiry_ms)
    }

    #[doc(hidden)]
    pub fn validate_extension_send(&self, now: u64) -> Result<()> {
        if self.extension_expired(now) {
            return Err(Error::Deadline);
        }
        Ok(())
    }

    pub fn managed_overrun(&self, actual: Amount) -> bool {
        (self.pricing.is_some() || self.extension.is_some())
            && (actual.tokens > self.reserve.tokens || actual.micro_usd > self.reserve.micro_usd)
    }

    pub(crate) fn matches_extension_receipt(&self, result: &ExternalResult) -> bool {
        match (&self.extension, result) {
            (
                Some(extension),
                ExternalResult::Applied {
                    external_id, usage, ..
                },
            ) => {
                external_id.as_deref() == Some(&extension.resource_id)
                    && usage.resource_expires_at_ms == Some(extension.expires_at_ms)
            }
            _ => true,
        }
    }

    /// A local preflight failure can release the reservation with explicit no-send evidence.
    pub fn settlement_result(&self, result: &ExternalResult) -> Result<Option<Amount>> {
        if let ExternalResult::Failed { usage, .. } = result
            && let crate::UsageOutcome::NonBillable(reason) = usage.outcome()
            && (self.extension.is_none()
                || reason != crate::NonBillableReason::RejectedBeforeExecution)
            && usage.quantities.values().all(|value| *value == 0)
            && usage.provider_allowance.is_none()
        {
            return Ok(Some(Amount::default()));
        }
        if let Some(extension) = &self.extension {
            match result {
                ExternalResult::Applied {
                    external_id: Some(id),
                    usage,
                    ..
                } if id == &extension.resource_id
                    && usage.resource_expires_at_ms == Some(extension.expires_at_ms) => {}
                ExternalResult::Failed {
                    provider_diagnostic: Some(diagnostic),
                    usage,
                    ..
                } if diagnostic.send_certainty == ProviderSendCertainty::DefinitelyNotSent
                    && usage.amount() == Ok(Amount::default()) => {}
                _ => return Ok(None),
            }
        }
        match result {
            ExternalResult::Failed {
                usage,
                provider_diagnostic: Some(diagnostic),
                ..
            } if self.pricing.is_some()
                && diagnostic.send_certainty == crate::ProviderSendCertainty::DefinitelyNotSent
                && usage.amount() == Ok(Amount::default()) =>
            {
                Ok(Some(Amount::default()))
            }
            ExternalResult::Applied { usage, .. } | ExternalResult::Failed { usage, .. } => {
                self.settlement_amount(usage)
            }
            _ => Ok(None),
        }
    }
    /// None means incomplete evidence, never measured zero.
    pub fn settlement_amount(&self, usage: &Usage) -> Result<Option<Amount>> {
        if usage.issue.is_some() {
            return Ok(None);
        }
        let mut amount = match usage.amount() {
            Ok(amount) => Some(amount),
            Err(Error::UsageUnknown) => None,
            Err(Error::Invalid(_)) => return Ok(None),
            Err(e) => return Err(e),
        };
        if !self
            .quantities
            .keys()
            .all(|k| usage.quantities.contains_key(k))
        {
            amount = None;
        }
        if let Some(snapshot) = &self.pricing {
            if self.price.is_some() {
                return Err(Error::Conflict);
            }
            return match snapshot.cost(usage) {
                Ok(reference) if amount == Some(reference) => Ok(amount),
                Ok(_) | Err(Error::UsageUnknown | Error::Invalid(_)) => Ok(None),
                Err(e) => Err(e),
            };
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
    /// store recomputes it from the original attempt.
    pub usage: Usage,
    /// External metering only. Catalog-backed recoveries use the stored snapshot.
    pub price: Option<Price>,
}
/// Audit of one recovery: the old usage, the price basis and the new usage.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementRecord {
    pub basis: String,
    pub price: Option<Price>,
    pub previous: Usage,
    pub settled: Usage,
    pub amount: Amount,
    /// Unix milliseconds on the host Clock.
    pub at: u64,
}
impl Operation {
    #[doc(hidden)]
    pub fn validate_result_send(&self) -> Result<()> {
        if self.request.extension.is_some() && self.send_count == 0 {
            return Err(Error::Conflict);
        }
        Ok(())
    }

    #[doc(hidden)]
    pub fn settlement_allowance(
        &self,
        result: &ExternalResult,
        now: u64,
    ) -> Result<Option<models::AllowanceCharge>> {
        let (usage, id) = match result {
            ExternalResult::Applied {
                usage, external_id, ..
            } => (usage, external_id.as_deref()),
            ExternalResult::Failed { usage, .. } => (usage, None),
            _ => return Ok(None),
        };
        let Some(evidence) = &usage.provider_allowance else {
            return Ok(None);
        };
        if self.send_count == 0
            || id != Some(evidence.receipt.as_str())
            || evidence.charged_at_ms < self.created_at
            || evidence.charged_at_ms > now
        {
            return Err(Error::Conflict);
        }
        let snapshot = self.request.pricing.as_ref().ok_or(Error::Conflict)?;
        if self.request.settlement_result(result)?.is_none() {
            return Ok(None);
        }
        snapshot.allowance_charge(usage, evidence).map(Some)
    }

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
            || recovery.usage.issue.is_some()
            || recovery.usage.non_billable.is_some()
        {
            return Err(Error::Invalid("settlement recovery".into()));
        }
        if let Some(done) = self.recovery.as_deref() {
            let mut requested = recovery.usage.clone();
            self.price_recovery(recovery, &mut requested)?;
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
        if stored.provider_allowance.is_some() || new.provider_allowance.is_some() {
            // Only the fenced record path can debit and settle together.
            return Err(Error::Conflict);
        }
        let keeps = |old: Option<u64>, new: Option<u64>| old.is_none() || old == new;
        if self.request.pricing.is_some()
            && stored.unknown_reason.as_deref() == Some("MANAGED_ROUTE_MISMATCH")
        {
            return Err(Error::UsageUnknown);
        }
        if stored.amount().is_ok()
            || stored.model != new.model
            || if self.request.pricing.is_some() {
                stored
                    .quantities
                    .iter()
                    .any(|(name, count)| new.quantities.get(name) != Some(count))
                    || new.quantities.keys().any(|name| {
                        !stored.quantities.contains_key(name)
                            && !matches!(
                                name.as_str(),
                                crate::models::CACHE_WRITE
                                    | crate::models::CACHE_WRITE_30M
                                    | crate::models::CACHE_WRITE_12H
                            )
                    })
            } else {
                stored.quantities != new.quantities
            }
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
            || stored.resource_expires_at_ms != new.resource_expires_at_ms
        {
            return Err(Error::Invalid(
                "recovery must only fill unknown counters".into(),
            ));
        }
        let mut settled = new.clone();
        self.price_recovery(recovery, &mut settled)?;
        if stored
            .micro_usd
            .is_some_and(|known| Some(known) != settled.micro_usd)
        {
            return Err(Error::Invalid("recovery must preserve known cost".into()));
        }
        if stored.price_evidence.as_ref().is_some_and(|old| {
            settled.price_evidence.as_ref().is_none_or(|evidence| {
                old.input_micro_usd
                    .is_some_and(|value| evidence.input_micro_usd != Some(value))
                    || old
                        .output_micro_usd
                        .is_some_and(|value| evidence.output_micro_usd != Some(value))
            })
        }) {
            return Err(Error::Invalid(
                "recovery must preserve known price evidence".into(),
            ));
        }
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
        if self.request.extension.is_some()
            && self.request.settlement_result(&result)? != Some(amount)
        {
            return Err(Error::UsageUnknown);
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

    fn price_recovery(&self, recovery: &SettlementRecovery, usage: &mut Usage) -> Result<()> {
        if let Some(snapshot) = &self.request.pricing {
            if recovery.price.is_some() {
                return Err(Error::Invalid(
                    "catalog price is pinned to the original attempt".into(),
                ));
            }
            usage.micro_usd = Some(snapshot.cost(usage)?.micro_usd);
            return Ok(());
        }
        let price = recovery.price.as_ref().ok_or(Error::PriceNotConfigured)?;
        if usage.model.is_empty()
            || price.model != usage.model
            || self
                .request
                .price
                .as_ref()
                .is_some_and(|pinned| pinned.model != usage.model)
        {
            return Err(Error::Invalid("recovery price/model mismatch".into()));
        }
        usage.micro_usd = Some(price.cost(usage)?);
        usage.price_evidence = Some(Box::new(price.cost_breakdown(usage)?));
        Ok(())
    }
}

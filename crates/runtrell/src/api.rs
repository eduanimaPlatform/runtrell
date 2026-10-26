// SPDX-License-Identifier: Apache-2.0

use crate::{AccountPolicy, PreparedModelRequest, ReservationPolicy, SchedulingPolicy};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub type Result<T> = std::result::Result<T, Error>;
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("conflicting digest or revision")]
    Conflict,
    #[error("owner generation or lease lost")]
    OwnershipLost,
    #[error("run closed")]
    Closed,
    #[error("deadline reached")]
    Deadline,
    #[error("budget at limit")]
    BudgetAtLimit,
    #[error("cannot safely reserve next call")]
    ReservationDenied,
    #[error("usage unavailable")]
    UsageUnknown,
    #[error("price not configured")]
    PriceNotConfigured,
    #[error("permission denied")]
    PermissionDenied,
    #[error("unsupported capability: {0}")]
    Unsupported(String),
    #[error("finite queue or slots full")]
    Backpressure,
    #[error("queue wait timed out")]
    QueueTimeout,
    #[error("machine wait: {0}")]
    Pending(String),
    #[error("not found")]
    NotFound,
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("storage failed")]
    Storage,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Amount {
    pub tokens: u64,
    pub micro_usd: u64,
}
impl Amount {
    pub const fn new(tokens: u64, micro_usd: u64) -> Self {
        Self { tokens, micro_usd }
    }
    pub fn checked_add(self, rhs: Self) -> Result<Self> {
        Ok(Self::new(
            self.tokens
                .checked_add(rhs.tokens)
                .ok_or(Error::ReservationDenied)?,
            self.micro_usd
                .checked_add(rhs.micro_usd)
                .ok_or(Error::ReservationDenied)?,
        ))
    }
    pub fn subtract(self, rhs: Self) -> Self {
        Self::new(self.tokens - rhs.tokens, self.micro_usd - rhs.micro_usd)
    }
    pub fn reaches(self, limit: Self) -> bool {
        self.tokens >= limit.tokens || self.micro_usd >= limit.micro_usd
    }
    pub fn exceeds(self, limit: Self) -> bool {
        self.tokens > limit.tokens || self.micro_usd > limit.micro_usd
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Total input tokens, including cached and image input.
    pub input: Option<u64>,
    /// Total output tokens, including reasoning and image output.
    pub output: Option<u64>,
    pub cached: Option<u64>,
    pub reasoning: Option<u64>,
    pub image_input: Option<u64>,
    pub image_output: Option<u64>,
    pub cached_image_input: Option<u64>,
    pub quantities: BTreeMap<String, u64>,
    pub micro_usd: Option<u64>,
    pub model: String,
}
impl Usage {
    pub fn measured(input: u64, output: u64, cost: u64) -> Self {
        Self {
            input: Some(input),
            output: Some(output),
            micro_usd: Some(cost),
            ..Self::default()
        }
    }
    pub fn validate_subsets(&self, i: u64, o: u64) -> Result<()> {
        let c = self.cached.unwrap_or(0);
        let image = self.image_input.unwrap_or(0);
        let cross = self.cached_image_input.unwrap_or(0);
        if c > i
            || image > i
            || self.reasoning.unwrap_or(0) > o
            || self.image_output.unwrap_or(0) > o
            || self.cached_image_input.is_some_and(|_| {
                cross > self.cached.unwrap_or(i).min(self.image_input.unwrap_or(i))
                    || u128::from(c) + u128::from(image) > u128::from(i) + u128::from(cross)
            })
        {
            return Err(Error::Invalid("usage subset".into()));
        }
        Ok(())
    }
    pub fn amount(&self) -> Result<Amount> {
        Ok(Amount::new(
            self.total()?,
            self.micro_usd.ok_or(Error::UsageUnknown)?,
        ))
    }
    /// Input + output, counting subsets only once; independent of measured cost.
    /// Missing totals or overflow are unknown, never measured zero.
    pub fn total(&self) -> Result<u64> {
        let i = self.input.ok_or(Error::UsageUnknown)?;
        let o = self.output.ok_or(Error::UsageUnknown)?;
        self.validate_subsets(i, o)?;
        i.checked_add(o).ok_or(Error::UsageUnknown)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Price {
    pub model: String,
    pub input: u64,
    pub cached: u64,
    pub output: u64,
    pub reasoning: u64,
    pub image_input: u64,
    pub image_output: u64,
    pub cached_image_input: u64,
}
impl Price {
    /// Conservative reservation for an input upper bound and output allowance.
    /// Host-supplied category rates are integer micro-USD per million tokens.
    /// Rounds up once for this bill; overflow rejects the reservation.
    pub fn estimate(&self, input_upper_bound: u64, output_allowance: u64) -> Result<Amount> {
        let (input_rate, output_rate) = self.maximum_rates();
        let numerator = (u128::from(input_upper_bound) * u128::from(input_rate))
            .checked_add(u128::from(output_allowance) * u128::from(output_rate))
            .ok_or(Error::ReservationDenied)?;
        Ok(Amount::new(
            input_upper_bound
                .checked_add(output_allowance)
                .ok_or(Error::ReservationDenied)?,
            u64::try_from(numerator.div_ceil(1_000_000)).map_err(|_| Error::ReservationDenied)?,
        ))
    }
    fn maximum_rates(&self) -> (u64, u64) {
        (
            self.input
                .max(self.cached)
                .max(self.image_input)
                .max(self.cached_image_input),
            self.output.max(self.reasoning).max(self.image_output),
        )
    }
    /// Integer micro-USD per million tokens, rounded up once per billed attempt.
    pub fn cost(&self, u: &Usage) -> Result<u64> {
        let i = u.input.ok_or(Error::UsageUnknown)?;
        let o = u.output.ok_or(Error::UsageUnknown)?;
        u.validate_subsets(i, o)?;
        // A missing category may be ignored only when it cannot affect price.
        let c = known_subset(
            u.cached,
            i,
            self.cached != self.input
                || (u.image_input != Some(0) && self.cached_image_input != self.image_input),
        )?
        .max(u.cached_image_input.unwrap_or(0));
        let image = known_subset(
            u.image_input,
            i,
            self.image_input != self.input
                || (u.cached != Some(0) && self.cached_image_input != self.cached),
        )?
        .max(u.cached_image_input.unwrap_or(0));
        let cross = known_subset(
            u.cached_image_input,
            c.min(image),
            i128::from(self.input) + i128::from(self.cached_image_input)
                != i128::from(self.cached) + i128::from(self.image_input),
        )?;
        let cross = if u.cached_image_input.is_none() {
            cross.max(c.saturating_sub(i - image))
        } else {
            cross
        };
        let mut r = known_subset(u.reasoning, o, self.reasoning != self.output)?;
        let mut image_out = known_subset(u.image_output, o, self.image_output != self.output)?;
        // Reasoning and image output are each a subset of output, but providers do
        // not promise they are disjoint and no overlap count is reported. A
        // category billed at the plain output rate cannot change the cost, so it
        // is folded into ordinary output; if both carry their own rate and both
        // are nonzero, the overlap decides the price and settlement is refused.
        if self.reasoning == self.output {
            r = 0;
        }
        if self.image_output == self.output {
            image_out = 0;
        }
        if r > 0 && image_out > 0 {
            return Err(Error::UsageUnknown);
        }
        // Inclusion/exclusion can have a signed coefficient; all final lines are nonnegative.
        let ordinary = i
            .checked_sub(c)
            .and_then(|n| n.checked_sub(image.saturating_sub(cross)))
            .ok_or_else(|| Error::Invalid("usage intersection".into()))?;
        let n = [
            (ordinary, self.input),
            (c - cross, self.cached),
            (image - cross, self.image_input),
            (cross, self.cached_image_input),
            (o - r - image_out, self.output),
            (r, self.reasoning),
            (image_out, self.image_output),
        ]
        .into_iter()
        .try_fold(0u128, |total, (tokens, rate)| {
            total
                .checked_add(u128::from(tokens) * u128::from(rate))
                .ok_or(Error::ReservationDenied)
        })?;
        u64::try_from(n.div_ceil(1_000_000)).map_err(|_| Error::ReservationDenied)
    }
}
fn known_subset(value: Option<u64>, total: u64, required: bool) -> Result<u64> {
    value
        .or_else(|| (total == 0 || !required).then_some(0))
        .ok_or(Error::UsageUnknown)
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelLimits {
    pub model: String,
    pub context: u64,
    pub max_output: u64,
    /// Technical minimum accepted max-output request parameter, not a guarantee
    /// of generated tokens. The host quality floor belongs to ReservationPolicy.
    pub min_max_output_tokens: u64,
    pub price: Option<Price>,
    pub timeout: u64,
}
impl ModelLimits {
    fn validated_price(&self) -> Result<&Price> {
        if self.context == 0
            || self.min_max_output_tokens == 0
            || self.min_max_output_tokens > self.max_output
            || self.max_output > self.context
        {
            return Err(Error::Invalid("model request bounds".into()));
        }
        let p = self.price.as_ref().ok_or(Error::PriceNotConfigured)?;
        if p.model != self.model {
            return Err(Error::PriceNotConfigured);
        }
        Ok(p)
    }
    /// Quote an explicit request without shrinking. Input is a host-supplied upper
    /// bound; output is a reserved allowance, not a promised generation length.
    pub fn estimate(&self, input_upper_bound: u64, output_allowance: u64) -> Result<Amount> {
        let p = self.validated_price()?;
        if output_allowance < self.min_max_output_tokens
            || output_allowance > self.max_output
            || input_upper_bound
                .checked_add(output_allowance)
                .is_none_or(|n| n > self.context)
        {
            return Err(Error::ReservationDenied);
        }
        p.estimate(input_upper_bound, output_allowance)
    }
    pub fn cap(&self, input: u64, remaining: Amount) -> Result<(u64, Amount)> {
        let p = self.validated_price()?;
        let (input_rate, output_rate) = p.maximum_rates();
        let input_cost = u128::from(input) * u128::from(input_rate);
        let money = (u128::from(remaining.micro_usd) * 1_000_000)
            .checked_sub(input_cost)
            .ok_or(Error::ReservationDenied)?;
        let money_cap = if output_rate == 0 {
            u64::MAX
        } else {
            u64::try_from(money / u128::from(output_rate)).unwrap_or(u64::MAX)
        };
        let cap = self
            .max_output
            .min(self.context.saturating_sub(input))
            .min(remaining.tokens.saturating_sub(input))
            .min(money_cap);
        if cap == 0 || cap < self.min_max_output_tokens {
            return Err(Error::ReservationDenied);
        }
        Ok((cap, self.estimate(input, cap)?))
    }
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RunHandle {
    pub tenant: String,
    pub id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunSpec {
    pub tenant: String,
    pub principal: String,
    pub request_key: String,
    pub account: String,
    pub parent: Option<RunHandle>,
    pub session: Option<String>,
    pub context_revision: u64,
    pub basis_revision: u64,
    pub config_version: String,
    pub input: String,
    pub images: Vec<crate::images::ImageReference>,
    pub purpose: String,
    pub deadline: Option<u64>,
    pub trigger: Trigger,
    pub validator: String,
    pub validator_version: String,
    pub budget: ReservationPolicy,
    pub scheduling: SchedulingPolicy,
    pub continuation: ContinuationPolicy,
    pub compaction: Option<CompactionPolicy>,
}
impl RunSpec {
    pub fn new(tenant: &str, key: &str, account: String, input: &str) -> Self {
        Self {
            tenant: tenant.into(),
            principal: "host".into(),
            request_key: key.into(),
            account,
            parent: None,
            session: None,
            context_revision: 0,
            basis_revision: 0,
            config_version: "1".into(),
            input: input.into(),
            images: vec![],
            purpose: "judgment".into(),
            deadline: None,
            trigger: Trigger::Api,
            validator: "host".into(),
            validator_version: "1".into(),
            budget: ReservationPolicy::default(),
            scheduling: SchedulingPolicy::default(),
            continuation: ContinuationPolicy::default(),
            compaction: None,
        }
    }
}
/// Successful token-limited responses start new, separately reserved turns.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContinuationPolicy {
    pub instruction: String,
    pub max_continuations: Option<u64>,
}
impl Default for ContinuationPolicy {
    fn default() -> Self {
        Self {
            instruction: "Continue from the previous response and complete the task.".into(),
            max_continuations: None,
        }
    }
}
/// Compact before a new model turn when context headroom falls below this value.
/// The host selects the summary model through ModelPort::compaction_limits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionPolicy {
    pub min_remaining_tokens: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Trigger {
    Api,
    Event,
    Schedule,
    CaseAppend,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Terminal {
    Succeeded,
    Failed(String),
    Cancelled(String),
    DeadlineReached,
    BudgetReached(String),
    OutcomeUnknown(String),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunState {
    Queued,
    Running,
    Waiting(String),
    Finalizing,
    Terminal(Terminal),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Progress {
    NotRequired,
    Pending,
    Confirmed,
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Delivery {
    None,
    Accepted(String),
    Confirmed(String),
    Superseded(String),
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    pub run: RunHandle,
    pub worker: String,
    pub generation: u64,
    pub lease_until: u64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub text: String,
    pub call_id: Option<String>,
    pub tool: Option<String>,
    pub data: Value,
}
impl Message {
    pub fn text(role: &str, text: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            text: text.into(),
            call_id: None,
            tool: None,
            data: Value::Null,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub schema: u32,
    pub revision: u64,
    pub payload: Value,
    pub messages: Vec<Message>,
    pub cursor: u64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunRecord {
    pub handle: RunHandle,
    pub spec: RunSpec,
    pub schema: u32,
    pub revision: u64,
    pub sequence: u64,
    pub owner: Option<Claim>,
    pub generation: u64,
    pub state: RunState,
    pub checkpoint: Option<Checkpoint>,
    pub cancel: Option<String>,
    pub stop: Progress,
    pub cleanup: Progress,
    pub settlement: Progress,
    pub delivery: Delivery,
    pub output: Option<Value>,
    pub receipt: Option<ValidationReceipt>,
    pub completion: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub tenant: String,
    pub parent: Option<String>,
    pub limit: Amount,
    pub reserved: Amount,
    pub used: Amount,
    pub unknown: u64,
    pub exhausted: bool,
    pub warnings: [u8; 2],
    pub policy: AccountPolicy,
    pub quantity_reserved: BTreeMap<String, u64>,
    pub quantity_used: BTreeMap<String, u64>,
    pub quantity_warnings: BTreeMap<String, u8>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum OperationKind {
    Run,
    Model,
    Tool,
    Compaction,
    ChildRun,
    Retry,
    Sandbox,
}
/// Opaque adapter proof, pinned atomically with the operation intent and reservation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryProof {
    pub bytes: Vec<u8>,
    pub generation: String,
    /// Absolute Clock timestamp. A later preparation must never extend an old reservation.
    pub expires_at: u64,
}
impl RecoveryProof {
    pub fn validate(&self, now: u64) -> Result<()> {
        if self.bytes.is_empty()
            || self.bytes.len() > 65_536
            || self.generation.is_empty()
            || self.generation.len() > 256
            || self.expires_at <= now
        {
            return Err(Error::Invalid("recovery proof bounds/expiry".into()));
        }
        Ok(())
    }
}
#[derive(Clone, Debug)]
pub struct PreparationContext {
    pub claim: Claim,
    pub principal: String,
    pub key: String,
    pub attempt: u32,
    pub target: String,
}
/// Physical sends reported by an adapter; distinct from logical usage_observations.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhysicalSendObservation {
    pub sequence: u32,
    pub bytes: Vec<u8>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EvidenceCompleteness {
    Complete,
    Truncated,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhysicalSendEvidence {
    pub observations: Vec<PhysicalSendObservation>,
    pub completeness: EvidenceCompleteness,
}
impl PhysicalSendEvidence {
    pub fn validate(&self) -> Result<()> {
        if self.observations.len() > 256
            || self
                .observations
                .iter()
                .map(|o| o.bytes.len())
                .sum::<usize>()
                > 65_536
            || self
                .observations
                .windows(2)
                .any(|p| p[0].sequence >= p[1].sequence)
        {
            return Err(Error::Invalid("physical send evidence bounds/order".into()));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DispatchRequest {
    pub recovery_proof: Option<RecoveryProof>,
    pub key: String,
    pub kind: OperationKind,
    pub target: String,
    pub version: String,
    pub account_binding: String,
    pub input: Value,
    pub reserve: Amount,
    pub quantities: BTreeMap<String, u64>,
    pub price: Option<Price>,
    pub scope: Option<String>,
    pub recovery_until: Option<u64>,
    pub attempt: u32,
    pub timeout: Option<u64>,
}
impl DispatchRequest {
    /// Prepared operations replay against the pinned proof generation. Tool scope
    /// remains the independently pinned descriptor digest.
    pub fn replay_scope(&self) -> Option<&str> {
        self.recovery_proof
            .as_ref()
            .map(|proof| proof.generation.as_str())
            .or(self.scope.as_deref())
    }
    pub fn new(
        key: &str,
        kind: OperationKind,
        target: &str,
        input: Value,
        reserve: Amount,
    ) -> Self {
        Self {
            recovery_proof: None,
            key: key.into(),
            kind,
            target: target.into(),
            version: "1".into(),
            account_binding: "host".into(),
            input,
            reserve,
            quantities: BTreeMap::new(),
            price: None,
            scope: None,
            recovery_until: None,
            attempt: 1,
            timeout: Some(30),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperationState {
    Intent,
    Sent,
    Accepted,
    Applied,
    Failed,
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ExternalResult {
    Applied {
        physical_evidence: Option<PhysicalSendEvidence>,
        output: Value,
        usage: Usage,
        external_id: Option<String>,
    },
    Accepted {
        external_id: String,
    },
    Failed {
        physical_evidence: Option<PhysicalSendEvidence>,
        code: String,
        usage: Usage,
        retry_after: Option<u64>,
    },
    Unknown,
}
impl ExternalResult {
    pub fn validate_evidence(&self) -> Result<()> {
        match self {
            Self::Applied {
                physical_evidence: Some(e),
                ..
            }
            | Self::Failed {
                physical_evidence: Some(e),
                ..
            } => e.validate(),
            _ => Ok(()),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Operation {
    pub id: String,
    pub run: RunHandle,
    pub request: DispatchRequest,
    pub state: OperationState,
    pub result: Option<ExternalResult>,
    pub settled: bool,
    pub unknown_usage: bool,
    pub send_generation: Option<u64>,
    pub send_count: u32,
    pub replay_proof_generation: Option<u64>,
    pub created_at: u64,
    pub finished_at: Option<u64>,
    pub usage_observations: Vec<Usage>,
    pub unsettled_result: Option<ExternalResult>,
}
// Keep lookup results inline: callers forward the receipt directly to atomic settlement.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Lookup {
    Found(ExternalResult),
    Pending,
    Unknown,
    NotFound { scope: String },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDescriptor {
    pub name: String,
    pub version: String,
    pub account: String,
    pub permission: String,
    pub capability: String,
    pub input_schema: Value,
    pub output_schema: Value,
    pub side_effect: bool,
    pub timeout: u64,
    pub max_output_bytes: usize,
    pub reserve: Amount,
    pub quantities: BTreeMap<String, u64>,
    pub conflict_scope: Option<String>,
    pub retry: RetryPolicy,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub backoff: u64,
    pub max_elapsed: u64,
}
#[derive(Clone, Debug)]
pub struct ToolContext {
    pub recovery_proof: Option<RecoveryProof>,
    pub claim: Claim,
    pub principal: String,
    pub operation: String,
    pub attempt: u32,
    pub deadline: Option<u64>,
    pub permission: String,
    pub account_binding: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDescriptor>,
    pub max_output: u64,
    pub metadata: Value,
    pub images: Vec<crate::images::ImageReference>,
    pub attachments: Vec<crate::attachments::Attachment>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelReply {
    pub text: String,
    pub calls: Vec<ToolCall>,
    pub complete: bool,
    pub metadata: Value,
}
/// JSON parser coordinates: one-based line/byte column (column 0 for empty input),
/// zero-based byte offset.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonPosition {
    pub line: usize,
    pub column: usize,
    pub byte_offset: usize,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub code: String,
    pub severity: String,
    /// Instance JSON Pointer (or business diagnostic location).
    pub path: String,
    /// Schema JSON Pointer for schema diagnostics.
    pub schema_path: Option<String>,
    pub artifact: Option<String>,
    pub file: Option<String>,
    pub field: Option<String>,
    /// Half-open byte span in the diagnostic target.
    pub span: Option<(u64, u64)>,
    pub position: Option<JsonPosition>,
    pub cause: String,
    pub hint: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ValidationReceipt {
    pub hash: String,
    pub basis_revision: u64,
    pub config_version: String,
    pub validator: String,
    pub validator_version: String,
    pub passed: bool,
    /// Trusted validator classification, bound to the same candidate and version.
    pub repairable: bool,
    pub diagnostics: Vec<Diagnostic>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Completion {
    pub id: String,
    pub output: Value,
    pub receipt: ValidationReceipt,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Warning {
    pub account: String,
    pub token_percent: Option<u8>,
    pub cost_percent: Option<u8>,
    pub quantity_percent: BTreeMap<String, u8>,
    pub used: Amount,
    pub limit: Amount,
}

pub trait Clock: Send + Sync {
    fn now(&self) -> u64;
    fn wait_until(&self, deadline: u64) -> impl std::future::Future<Output = ()> + Send;
}
pub trait Policy: Send + Sync {
    fn authorize(&self, run: &RunRecord, target: &str, permission: &str) -> Result<()>;
}
pub trait ModelPort: Send + Sync {
    /// Pure admission check for a tool exchange, without resolving payloads or
    /// sending inference. Return MODEL_ATTACHMENT_LIMIT for repairable size/count
    /// failures so the driver can ask the agent to narrow the view.
    fn check_input_attachments(
        &self,
        _attachments: &[crate::attachments::Attachment],
        _images: &[crate::images::ImageReference],
    ) -> Result<()> {
        Ok(())
    }
    /// A host port may route summary requests to a different configured model.
    /// prepare/invoke/lookup must route by the request's model, including recovery.
    fn compaction_limits(&self) -> ModelLimits {
        self.limits()
    }
    /// Fix attachment identity and prepare authorized local bytes before sizing
    /// and reservation. External transport resolution may wait until invoke.
    fn prepare_request(
        &self,
        _run: &RunRecord,
        request: ModelRequest,
    ) -> impl std::future::Future<Output = Result<PreparedModelRequest>> + Send {
        async move { PreparedModelRequest::text(request) }
    }
    /// Only declared adapters are prepared; ordinary local tools need no proof.
    fn requires_recovery_preparation(&self) -> bool {
        false
    }
    fn prepare_recovery(
        &self,
        _context: &PreparationContext,
        _input: &Value,
    ) -> impl std::future::Future<Output = Result<Option<RecoveryProof>>> + Send {
        async move { Ok(None) }
    }

    fn cancel(
        &self,
        _context: &ToolContext,
    ) -> impl std::future::Future<Output = Result<Progress>> + Send {
        async move { Ok(Progress::Unknown) }
    }
    fn retry_policy(&self) -> RetryPolicy {
        RetryPolicy {
            max_attempts: 3,
            backoff: 1,
            max_elapsed: 60,
        }
    }
    fn limits(&self) -> ModelLimits;
    fn input_upper_bound(&self, request: &PreparedModelRequest) -> Result<u64>;
    fn recovery(&self) -> Option<(String, u64)> {
        None
    }
    fn invoke(
        &self,
        context: &ToolContext,
        request: PreparedModelRequest,
    ) -> impl std::future::Future<Output = Result<ExternalResult>> + Send;
    fn lookup(
        &self,
        context: &ToolContext,
        request: PreparedModelRequest,
    ) -> impl std::future::Future<Output = Result<Lookup>> + Send;
}
pub trait Tool: Send + Sync {
    fn authorize_result(
        &self,
        _run: &RunRecord,
        _output: &Value,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        async move { Ok(()) }
    }
    /// Preflight the complete request and quote any floor above the descriptor's
    /// configured reserve. Runtime reserves the maximum of both.
    fn quote(
        &self,
        _run: &RunRecord,
        _input: &Value,
    ) -> impl std::future::Future<Output = Result<Amount>> + Send {
        async move { Ok(Amount::default()) }
    }
    /// Resource admission class; execution tools reserve Sandbox slots.
    fn operation_kind(&self) -> OperationKind {
        OperationKind::Tool
    }
    /// Only declared adapters are prepared; ordinary local tools need no proof.
    fn requires_recovery_preparation(&self) -> bool {
        false
    }
    fn prepare_recovery(
        &self,
        _context: &PreparationContext,
        _input: &Value,
    ) -> impl std::future::Future<Output = Result<Option<RecoveryProof>>> + Send {
        async move { Ok(None) }
    }

    fn cancel(
        &self,
        _context: &ToolContext,
        _input: Value,
    ) -> impl std::future::Future<Output = Result<Progress>> + Send {
        async move { Ok(Progress::Unknown) }
    }
    fn descriptor(&self) -> ToolDescriptor;
    fn invoke(
        &self,
        context: &ToolContext,
        input: Value,
    ) -> impl std::future::Future<Output = Result<ExternalResult>> + Send;
    fn lookup(
        &self,
        context: &ToolContext,
        input: Value,
    ) -> impl std::future::Future<Output = Result<Lookup>> + Send;
}
/// Host supplies limits and business-specific diagnostic classification.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RepairPolicy {
    pub max_repairs: Option<u64>,
    pub max_elapsed: Option<u64>,
}
pub trait Validator: Send + Sync {
    fn repair_policy(&self) -> RepairPolicy {
        RepairPolicy::default()
    }
    fn validate(&self, run: &RunRecord, candidate: &Value) -> Result<ValidationReceipt>;
}
pub trait Finalizer: Send + Sync {
    fn finalize(
        &self,
        run: &RunRecord,
        candidate: Value,
        receipt: ValidationReceipt,
    ) -> Result<Completion>;
}
pub trait SecretProvider: Send + Sync {
    fn with_secret<R: Send>(
        &self,
        context: &ToolContext,
        target: &str,
        use_secret: impl FnOnce(&[u8]) -> Result<R> + Send,
    ) -> impl std::future::Future<Output = Result<R>> + Send;
}
pub trait Observer {
    fn observe(&self, event: &Event);
}
#[derive(Clone, Debug)]
pub struct Event {
    pub run: RunHandle,
    pub operation: Option<String>,
    pub revision: u64,
    pub code: String,
}

pub trait RunStore: Send + Sync {
    fn renew(
        &self,
        claim: &Claim,
        now: u64,
        lease: u64,
    ) -> impl std::future::Future<Output = Result<Claim>> + Send;
    fn find_request(
        &self,
        spec: &RunSpec,
    ) -> impl std::future::Future<Output = Result<Option<RunHandle>>> + Send;
    fn start(&self, spec: RunSpec) -> impl std::future::Future<Output = Result<RunHandle>> + Send;
    fn get(&self, run: &RunHandle) -> impl std::future::Future<Output = Result<RunRecord>> + Send;
    fn claim(
        &self,
        run: &RunHandle,
        worker: &str,
        now: u64,
        lease: u64,
    ) -> impl std::future::Future<Output = Result<Claim>> + Send;
    fn check(
        &self,
        claim: &Claim,
        now: u64,
        new_work: bool,
    ) -> impl std::future::Future<Output = Result<RunRecord>> + Send;
    fn checkpoint(
        &self,
        claim: &Claim,
        expected: u64,
        checkpoint: Checkpoint,
        now: u64,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
    fn cancel(
        &self,
        run: &RunHandle,
        reason: &str,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
    fn terminate(
        &self,
        claim: &Claim,
        terminal: Terminal,
        now: u64,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
    fn finalize(
        &self,
        claim: &Claim,
        completion: Completion,
        now: u64,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
    fn wait(
        &self,
        claim: &Claim,
        reason: &str,
        now: u64,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
    fn cleanup(
        &self,
        claim: &Claim,
        stop: Progress,
        cleanup: Progress,
        now: u64,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
}
pub trait BudgetAuthority: Send + Sync {
    fn create_account_with_policy(
        &self,
        tenant: &str,
        limit: Amount,
        parent: Option<String>,
        policy: AccountPolicy,
    ) -> impl std::future::Future<Output = Result<String>> + Send;
    /// Fenced, zero-charge finalization admission; does not authorize new I/O.
    fn finalization_allowed(
        &self,
        claim: &Claim,
        now: u64,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
    fn exhaust(
        &self,
        claim: &Claim,
        now: u64,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
    fn create_account(
        &self,
        tenant: &str,
        limit: Amount,
        parent: Option<String>,
    ) -> impl std::future::Future<Output = Result<String>> + Send;
    fn account(&self, id: &str) -> impl std::future::Future<Output = Result<Account>> + Send;
    fn remaining(
        &self,
        claim: &Claim,
        now: u64,
    ) -> impl std::future::Future<Output = Result<Amount>> + Send;
    fn warnings(
        &self,
        claim: &Claim,
        now: u64,
    ) -> impl std::future::Future<Output = Result<Vec<Warning>>> + Send;
}
pub trait OperationStore: Send + Sync {
    fn observe_result(
        &self,
        claim: &Claim,
        id: &str,
        result: ExternalResult,
        now: u64,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
    fn proven_unsent(
        &self,
        claim: &Claim,
        id: &str,
        scope: &str,
        now: u64,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
    /// Atomic reservation across all ancestors plus intent; no I/O before success.
    fn begin(
        &self,
        claim: &Claim,
        request: DispatchRequest,
        now: u64,
    ) -> impl std::future::Future<Output = Result<Operation>> + Send;
    fn list_operations(
        &self,
        run: &RunHandle,
    ) -> impl std::future::Future<Output = Result<Vec<Operation>>> + Send;
    fn operation(
        &self,
        run: &RunHandle,
        key: &str,
    ) -> impl std::future::Future<Output = Result<Option<Operation>>> + Send;
    /// Every send, including a proven-not-sent replay, requires a fresh fence.
    fn mark_sent(
        &self,
        claim: &Claim,
        id: &str,
        now: u64,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
    /// Atomic result and settlement. Existing operations remain writable after cancellation.
    fn record(
        &self,
        claim: &Claim,
        id: &str,
        result: ExternalResult,
        now: u64,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotRequest {
    pub key: String,
    pub tenant: String,
    pub run: String,
    pub kind: OperationKind,
}
#[derive(Clone, Debug)]
pub struct QueueEntry {
    pub key: String,
    pub tenant: String,
    pub scheduling: SchedulingPolicy,
    pub deadline: u64,
}
#[derive(Clone, Debug)]
pub struct Capacity {
    pub global: BTreeMap<OperationKind, usize>,
    pub tenant: BTreeMap<OperationKind, usize>,
    pub run: BTreeMap<OperationKind, usize>,
    pub queue: usize,
    pub tenant_queue: usize,
}
impl Default for Capacity {
    fn default() -> Self {
        let limits = BTreeMap::from([
            (OperationKind::Run, 16),
            (OperationKind::Model, 8),
            (OperationKind::Tool, 8),
            (OperationKind::Sandbox, 2),
            (OperationKind::ChildRun, 4),
            (OperationKind::Retry, 8),
            (OperationKind::Compaction, 2),
        ]);
        Self {
            global: limits.clone(),
            tenant: limits.clone(),
            run: limits,
            queue: 64,
            tenant_queue: 16,
        }
    }
}
pub trait Scheduler: Send + Sync {
    fn acquire(&self, request: SlotRequest)
    -> impl std::future::Future<Output = Result<()>> + Send;
    fn release_slot(&self, key: &str) -> impl std::future::Future<Output = Result<()>> + Send;
    fn enqueue(&self, entry: QueueEntry) -> impl std::future::Future<Output = Result<()>> + Send;
    fn dequeue(
        &self,
        now: u64,
    ) -> impl std::future::Future<Output = Result<Option<QueueEntry>>> + Send;
}
pub trait ControlStore: RunStore + BudgetAuthority + OperationStore + Scheduler {}
impl<T: RunStore + BudgetAuthority + OperationStore + Scheduler> ControlStore for T {}

/// Content identity for structured decisions and validation receipts.
/// Object key order is not semantic and may change through durable JSON stores.
pub fn digest(value: &Value) -> String {
    use sha2::{Digest, Sha256};
    let mut value = value.clone();
    value.sort_all_objects();
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&value).expect("JSON value serializes"))
    )
}

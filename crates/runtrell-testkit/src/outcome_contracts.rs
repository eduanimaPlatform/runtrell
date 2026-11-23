// SPDX-License-Identifier: Apache-2.0
use crate::*;

#[derive(Clone, Copy, Debug)]
pub enum OutcomeCase {
    Rejection(u16),
    ErrorEnvelope,
    FailureWithUsage,
    FailureWithPartialUsage,
    FailureWithoutUsage,
    FailureWithInvalidUsage,
    Dns,
    Connect,
    Tls,
    Builder,
    RequestByteLimit,
    DeadlineBeforePoll,
    Disconnect,
    TruncatedRejection,
    OversizedRejection,
    TruncatedSuccess,
    TruncatedFailure,
    InvalidJson,
    InvalidSchema,
    InvalidSuccessUsage,
    ResponseByteLimit,
    InvalidToolInput,
    PermissionDenied,
}

pub const OUTCOME_CASES: &[OutcomeCase] = &[
    OutcomeCase::Rejection(400),
    OutcomeCase::Rejection(401),
    OutcomeCase::Rejection(402),
    OutcomeCase::Rejection(403),
    OutcomeCase::Rejection(404),
    OutcomeCase::Rejection(413),
    OutcomeCase::Rejection(422),
    OutcomeCase::Rejection(429),
    OutcomeCase::ErrorEnvelope,
    OutcomeCase::FailureWithUsage,
    OutcomeCase::FailureWithPartialUsage,
    OutcomeCase::FailureWithoutUsage,
    OutcomeCase::FailureWithInvalidUsage,
    OutcomeCase::Dns,
    OutcomeCase::Connect,
    OutcomeCase::Tls,
    OutcomeCase::Builder,
    OutcomeCase::RequestByteLimit,
    OutcomeCase::DeadlineBeforePoll,
    OutcomeCase::Disconnect,
    OutcomeCase::TruncatedRejection,
    OutcomeCase::OversizedRejection,
    OutcomeCase::TruncatedSuccess,
    OutcomeCase::TruncatedFailure,
    OutcomeCase::InvalidJson,
    OutcomeCase::InvalidSchema,
    OutcomeCase::InvalidSuccessUsage,
    OutcomeCase::ResponseByteLimit,
    OutcomeCase::InvalidToolInput,
    OutcomeCase::PermissionDenied,
];

#[derive(Debug, PartialEq, Eq)]
pub struct OutcomeException {
    pub key: String,
    pub kind: OperationKind,
    pub state: OperationState,
    pub ambiguity: Option<AmbiguityReason>,
    pub usage_issue: Option<UsageIssue>,
    pub unknown_usage: bool,
    pub unsettled: bool,
    pub operator_action: bool,
}

/// Check the durable outcome, the usage evidence and the operator gate at a
/// test checkpoint. Every listed exception must occur exactly once. Call with
/// an empty slice for a completed fixture with no unresolved state.
pub async fn assert_no_unexpected_ambiguity(
    store: &impl ControlStore,
    run: &RunHandle,
    expected: &[OutcomeException],
    operator_wait: bool,
) {
    let record = store.get(run).await.expect("read test run");
    let operations = store
        .list_operations(run)
        .await
        .expect("read test operations");
    let mut actual = Vec::new();
    for op in operations {
        let ambiguity = op
            .result
            .as_ref()
            .and_then(ExternalResult::ambiguity)
            .or_else(|| {
                op.unsettled_result
                    .as_ref()
                    .and_then(ExternalResult::ambiguity)
            })
            .map(AmbiguousOutcome::reason);
        let usage_issue = match op.usage_outcome() {
            UsageOutcome::Unresolved { reason, .. } => Some(reason),
            _ => None,
        };
        let operator_action = op.awaits_operator();
        if ambiguity.is_some()
            || usage_issue.is_some()
            || op.unknown_usage
            || !op.settled
            || operator_action
            || op.state == OperationState::Unknown
        {
            actual.push(OutcomeException {
                key: op.request.key,
                kind: op.request.kind,
                state: op.state,
                ambiguity,
                usage_issue,
                unknown_usage: op.unknown_usage,
                unsettled: !op.settled,
                operator_action,
            });
        }
    }
    assert_eq!(
        actual.len(),
        expected.len(),
        "unexpected outcome exceptions: {actual:?}"
    );
    for exception in expected {
        let index = actual
            .iter()
            .position(|item| item == exception)
            .unwrap_or_else(|| panic!("expected {exception:?}, observed {actual:?}"));
        actual.swap_remove(index);
    }
    assert_eq!(
        record.state == RunState::Waiting(OPERATOR_ACTION_WAIT.into()),
        operator_wait,
        "unexpected operator wait"
    );
    if matches!(
        record.state,
        RunState::Terminal(Terminal::OutcomeUnknown(_))
    ) {
        assert!(
            !expected.is_empty(),
            "unexpected terminal outcome ambiguity"
        );
    }
}

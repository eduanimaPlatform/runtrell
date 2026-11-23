// SPDX-License-Identifier: Apache-2.0
//! Outcome fixtures for durable run tests.
//!
//! Bind a guard for the full test, before opening its stores. It checks final
//! persisted state after the test has joined its workers and finished recovery.
//! Declare intentional unresolved work with an exact per-run expectation.

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

pub use runtrell::test_outcomes::{
    ExpectedOutcome, OutcomeException, assert_no_unexpected_ambiguity, expect, expect_operations,
    expect_operator_wait, guard,
};

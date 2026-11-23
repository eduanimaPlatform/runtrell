// SPDX-License-Identifier: Apache-2.0
//! Test fixtures for checking durable run outcomes.

use crate::*;
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    future::Future,
    marker::PhantomData,
    rc::Rc,
};

#[derive(Debug, PartialEq, Eq)]
pub struct OutcomeException {
    pub key: String,
    pub kind: OperationKind,
    pub attempt: u32,
    pub state: OperationState,
    pub ambiguity: Option<AmbiguityReason>,
    pub usage_issue: Option<UsageIssue>,
    pub unknown_usage: bool,
    pub unsettled: bool,
    pub operator_action: bool,
}

impl OutcomeException {
    pub fn unsettled(key: &str, kind: OperationKind, state: OperationState) -> Self {
        Self {
            key: key.into(),
            kind,
            attempt: 1,
            state,
            ambiguity: None,
            usage_issue: None,
            unknown_usage: false,
            unsettled: true,
            operator_action: false,
        }
    }

    pub fn at_attempt(mut self, attempt: u32) -> Self {
        self.attempt = attempt;
        self
    }

    pub fn unresolved_usage(
        key: &str,
        kind: OperationKind,
        state: OperationState,
        issue: UsageIssue,
    ) -> Self {
        Self {
            usage_issue: Some(issue),
            unknown_usage: true,
            ..Self::unsettled(key, kind, state)
        }
    }

    pub fn unresolved(key: &str, kind: OperationKind, reason: AmbiguityReason) -> Self {
        Self {
            ambiguity: Some(reason),
            usage_issue: Some(UsageIssue::OutcomeUnresolved),
            unknown_usage: true,
            ..Self::unsettled(key, kind, OperationState::Unknown)
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ExpectedOutcome {
    pub exceptions: Vec<OutcomeException>,
    pub operator_wait: bool,
    pub terminal_unknown: bool,
}

pub struct ObservedRun {
    handle: RunHandle,
    request: String,
    outcome: ExpectedOutcome,
}

/// Classify persisted state with the same rules as the checkpoint assertion.
pub fn observe(
    handle: RunHandle,
    request: String,
    state: &RunState,
    operations: impl IntoIterator<Item = Operation>,
) -> ObservedRun {
    let mut exceptions = Vec::new();
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
            exceptions.push(OutcomeException {
                key: op.request.key,
                kind: op.request.kind,
                attempt: op.request.attempt,
                state: op.state,
                ambiguity,
                usage_issue,
                unknown_usage: op.unknown_usage,
                unsettled: !op.settled,
                operator_action,
            });
        }
    }
    ObservedRun {
        handle,
        request,
        outcome: ExpectedOutcome {
            exceptions,
            operator_wait: matches!(state, RunState::Waiting(reason) if reason == OPERATOR_ACTION_WAIT),
            terminal_unknown: matches!(state, RunState::Terminal(Terminal::OutcomeUnknown(_))),
        },
    }
}

fn same_exceptions(actual: &[OutcomeException], expected: &[OutcomeException]) -> bool {
    if actual.len() != expected.len() {
        return false;
    }
    let mut unmatched: Vec<_> = actual.iter().collect();
    for exception in expected {
        let Some(index) = unmatched.iter().position(|item| *item == exception) else {
            return false;
        };
        unmatched.swap_remove(index);
    }
    true
}

/// Compare the durable outcome, usage evidence and operator gate at a checkpoint.
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
    let actual = observe(
        run.clone(),
        record.spec.request_key,
        &record.state,
        operations,
    )
    .outcome;
    assert_eq!(
        actual.operator_wait, operator_wait,
        "unexpected operator wait"
    );
    assert!(
        !actual.terminal_unknown || !expected.is_empty(),
        "unexpected terminal outcome ambiguity"
    );
    assert!(
        same_exceptions(&actual.exceptions, expected),
        "unexpected outcome exceptions for {run:?}: expected {expected:?}, observed {:?}",
        actual.exceptions
    );
}

type Scan = Box<dyn FnOnce(&tokio::runtime::Runtime) -> Vec<ObservedRun> + Send>;

#[derive(Default)]
struct Scope {
    scans: Vec<Scan>,
    stores: BTreeSet<String>,
    expected: BTreeMap<RunHandle, ExpectedOutcome>,
}

thread_local! {
    static ACTIVE: RefCell<Option<Scope>> = const { RefCell::new(None) };
}

/// Hold this on the test's main thread until its work and recovery have finished.
/// Store constructors register with this scope. Drop checks their final state,
/// including child runs and changes made through independently reopened stores.
#[must_use]
pub struct OutcomeGuard(PhantomData<Rc<()>>);

pub fn guard() -> OutcomeGuard {
    ACTIVE.with_borrow_mut(|active| {
        assert!(active.is_none(), "an outcome guard is already active");
        *active = Some(Scope::default());
    });
    OutcomeGuard(PhantomData)
}

/// Declare the precise unresolved state this test intends to leave for this run.
pub fn expect(run: &RunHandle, outcome: ExpectedOutcome) {
    assert_ne!(
        outcome,
        ExpectedOutcome::default(),
        "empty outcome exception"
    );
    ACTIVE.with_borrow_mut(|active| {
        let previous = active
            .as_mut()
            .expect("outcome guard required")
            .expected
            .insert(run.clone(), outcome);
        assert!(previous.is_none(), "outcome already declared for {run:?}");
    });
}

pub fn expect_operations(run: &RunHandle, exceptions: impl Into<Vec<OutcomeException>>) {
    expect(
        run,
        ExpectedOutcome {
            exceptions: exceptions.into(),
            ..ExpectedOutcome::default()
        },
    );
}

pub fn expect_operator_wait(run: &RunHandle) {
    expect(
        run,
        ExpectedOutcome {
            operator_wait: true,
            ..ExpectedOutcome::default()
        },
    );
}

pub fn track_store<F, Fut>(identity: Option<String>, snapshot: F)
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = Vec<ObservedRun>> + 'static,
{
    ACTIVE.with_borrow_mut(|active| {
        let Some(scope) = active else { return };
        if let Some(identity) = identity
            && !scope.stores.insert(identity)
        {
            return;
        }
        scope
            .scans
            .push(Box::new(move |runtime| runtime.block_on(snapshot())));
    });
}

impl Drop for OutcomeGuard {
    fn drop(&mut self) {
        let mut scope = ACTIVE.with_borrow_mut(|active| active.take().expect("outcome guard"));
        // A failing test already has its own diagnostic. Avoid a second panic
        // while unwinding and leave the original test failure intact.
        if std::thread::panicking() {
            return;
        }
        if scope.scans.is_empty() && scope.expected.is_empty() {
            return;
        }

        let runs = std::thread::spawn(move || {
            // Run independently of the test's executor (which can be paused or
            // have shut down) and of any SQL connection closed by the test.
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("start outcome checker");
            scope
                .scans
                .into_iter()
                .flat_map(|scan| scan(&runtime))
                .collect::<Vec<_>>()
        })
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic));

        let mut mismatches = Vec::new();
        for run in runs {
            let expected = scope.expected.remove(&run.handle).unwrap_or_default();
            if run.outcome.operator_wait != expected.operator_wait
                || run.outcome.terminal_unknown != expected.terminal_unknown
                || !same_exceptions(&run.outcome.exceptions, &expected.exceptions)
            {
                mismatches.push(format!(
                    "run {:?}: expected {expected:?}, observed {:?}",
                    run.request, run.outcome
                ));
            }
        }
        assert!(
            mismatches.is_empty() && scope.expected.is_empty(),
            "unexpected outcome exceptions:\n{}\nunmatched declarations: {:?}",
            mismatches.join("\n"),
            scope.expected
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "unexpected outcome exceptions")]
    fn undeclared_terminal_unknown_fails_on_scope_exit() {
        let _outcomes = guard();
        let store = crate::memory::MemoryStore::default();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(async {
            let account = store
                .create_account("tenant", Amount::new(10, 10), None)
                .await
                .unwrap();
            let run = store
                .start(RunSpec::new("tenant", "request", account, "test"))
                .await
                .unwrap();
            let claim = store.claim(&run, "worker", 0, 10).await.unwrap();
            store
                .terminate(
                    &claim,
                    Terminal::OutcomeUnknown("delivery unverified".into()),
                    1,
                )
                .await
                .unwrap();
        });
    }
}

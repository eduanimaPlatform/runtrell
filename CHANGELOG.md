# Changelog

## Unreleased — explicit transient model retries (breaking)

- Provider `Config` now requires a `retry_policy`; the default permits three total attempts,
  1,000 ms initial backoff, and 60,000 ms total elapsed time. The policy is pinned in Run
  checkpoints.
- Only documented OpenAI Responses 429 `slow_down` and 503 `server_is_overloaded` failures
  enable automatic retry. Each retry has its own reservation and attempt; unknown usage keeps
  its reservation and slot until owner settlement, even if a later attempt succeeds.
- A retry that expires after admission but before any send retains a settled no-send failure
  record and releases only its own reservation and slot. An oversized `Retry-After` cannot
  shorten the server delay.

## Unreleased — text-only model file reads (breaking)

- `files.read` and `artifacts.read` return a UTF-8 `text` range to the model.
  Binary artifacts return `BINARY_ARTIFACT` with bounded metadata and guidance;
  invalid UTF-8 or binary controls are rejected. Host storage APIs still return
  raw bytes. The file descriptor version is `runtrell-files-4`.
- Stored output previews use text and trim an incomplete UTF-8 character.
  Their descriptor version is `stored-output-2`. `Tool::authorize_input` is
  called on new tool calls and saved-result replay so a revoked grant cannot
  expose a stored failure hint.

## Unreleased — configurable no-progress stop (breaking)

- `RepairPolicy.max_no_progress` is now `Option<NonZeroU64>`: the default is
  `Some(3)`, an explicit `None` disables the repeated-failure count, and
  `Some(N)` sets another threshold. This applies to output validation and tool
  input/execution repair. Rust callers that construct the field must use the
  option type; serialized policies must explicitly include the field.
- The full repair policy is pinned in Run checkpoints. A resumed runtime with a
  different policy is refused before further model or tool work. Previously
  saved checkpoints without the field are not reopened; no migration is provided.
  Budget, deadline, cancellation and authorization gates remain in effect.

## Unreleased — partial price evidence (breaking)

- Add `Price::cost_breakdown` and optional `Usage.price_evidence` with a pinned
  price and independently known input/output micro-USD components. Components
  round independently for audit and are not additive billing amounts. OpenAI
  Images records them even when missing cache data keeps the full bill unknown;
  SVG author cost remains in `SvgUsage`. Owner recovery recomputes components,
  preserves known evidence, and still settles the complete attempt only once.
  Rust callers constructing `Usage` must initialize `price_evidence` or use
  `..Usage::default()`; no consumer revision or stored operation is changed here.

## Unreleased — owner commands (breaking)

- `OperationStore::resume_held` now takes an expected Run revision, `OwnerCommand`
  (host key and request digest), and Unix-millisecond `now`; it returns an immutable
  `OwnerReceipt`. Cancellation/terminal state, a stale revision, missing hold, live
  claim, reached deadline, unresolved operator work, and ancestor account admission
  or fully reserved capacity refuse atomically without changing the Run.
- `settle_recovered` also requires `OwnerCommand` and returns `OwnerReceipt` with
  `OwnerResult::Settled`; settlement still never resumes a Run. Implementations of
  `OperationStore` must update both methods and add read-only `owner_receipt(key)`.
  Same key/digest/typed request returns the original receipt despite later Run
  transitions; a changed digest or request conflicts. A new key cannot attribute an
  already settled operation to itself.
- Memory and Postgres persist the receipt together with its effect. Postgres adds
  `0002_owner_receipts.sql` without modifying `0001.sql`; initialize before using
  the new adapter (`connect` refuses a missing receipt table). Receipt UPDATE and
  DELETE are rejected. `PgStore::{resume_held_in,settle_recovered_in}` let hosts
  commit their own audit in the same transaction, using the same transitions.

## Unreleased

- OpenAI Images usage and unsettled-operation recovery. `OpenAiImages` now normalizes the
  `ImagesUsage` response itself (`runtrell_providers::openai_images_usage`) instead of the
  Responses parser: cached counters are exactly 0 only for `gpt-image-2` and `gpt-image-2.<n>`
  models (the vendor states cached pricing does not apply to direct Images API requests) and
  stay unknown for other models with a distinct `Usage.unknown_reason`. Add
  `ImageCapabilities.response_model_aliases`. `Price` documents its four independent input lines.
  An Applied/Failed operation whose saved usage cannot settle now puts the Run in
  `Waiting(OPERATOR_ACTION_WAIT)` once it has no lookup window left, and the driver stops
  reconciling it. New `OperationStore::settle_recovered` (implemented by Memory and Pg; **hosts
  that implement `OperationStore` must add it**) settles the original operation once from a
  `SettlementRecovery` and records old/new usage and price basis in `Operation.recovery`; the
  Postgres schema gains `attempts.recovery` (added by `initialize` to an existing runtime;
  `connect` requires it). `Usage` gains `unknown_reason`.
  `settle_recovered` never resumes the Run: it leaves an operator hold (state `Waiting(OPERATOR_ACTION_WAIT)`,
  owner cleared) that only the new `OperationStore::resume_held` lifts; hosts implementing
  `OperationStore` must add `resume_held`.
  A provider result that is missing or incomplete (a model reply with `complete: false`, or no
  input/output totals) is not parked: the reopened Run still looks it up once and either recovers
  the reply or ends `OutcomeUnknown`. A Run parks only after a lookup returned the same evidence.

- Time-unit contract: every `u64` time and duration in the control API is
  milliseconds (absolute values are Unix milliseconds in the host `Clock` epoch).
  `Clock`, `RunSpec.deadline`, leases, operation `timeout`, `RetryPolicy`,
  `retry_after`, `recovery_until`, retention and queue deadlines are documented in
  place and in docs/design/api.md. **Breaking for hosts that used seconds:** the
  defaults changed to milliseconds (`DispatchRequest::new` timeout 30_000, default
  `RetryPolicy` backoff 1_000 / max_elapsed 60_000, file-tool timeout 30_000,
  `DocumentViewLimits.timeout` 60_000); `runtrell-providers` now reads
  `ModelLimits.timeout` as milliseconds, compares `ToolContext.deadline` with Unix
  milliseconds, and converts HTTP `Retry-After` to milliseconds. The capacity-queue
  default wait for a Run without a deadline was `now + 60` (60 ms under a
  millisecond clock) and is now `CAPACITY_WAIT_MS` = 60_000. Add fake-clock
  boundary contracts (`runtrell-testkit::time_contracts`, retention tests).
- Split PostgreSQL startup: `PgStore::open` is replaced by explicit
  `PgStore::initialize` (schema, migration, `schema_config`, capacity seed; same
  idempotent behavior) and `PgStore::connect`, which verifies the existing
  schema version, tables and Capacity in one read-only transaction without DDL or
  writes. `connect` needs only `USAGE` on the schema and `SELECT` on
  `schema_config` and returns `NotFound`, `PermissionDenied`, `Unsupported` or
  `Conflict` on a missing or mismatched runtime. Hosts must rename `open` calls to
  `initialize` and use `connect` for processes that must not change the schema.

- Declare file-tool argument bounds from the active StoragePolicy and shared
  DataStore path/query constants. `runtrell-files-3` adds bounded list/search
  counts, read lengths, paths, text and edit arrays. Schema violations return
  through existing tool repair; runtime `PAGE_LIMIT`, `RANGE_LIMIT`,
  `SEARCH_LIMIT` and `EDIT_LIMIT` return safe hints with public policy ranges.
  UTF-8 byte and aggregate checks remain enforced at execution. Storage
  capacity, serialized input/output limits, authority and storage failures
  retain terminal behavior. Add shared Memory/Pg empty-query and UTF-8 repair
  regressions plus nondefault-policy schema/store boundary tests.

- Add required `Diagnostic.repair: DiagnosticRepair` and
  `Diagnostic.observation: Option<ArtifactObservation>`. Hosts declare Agent,
  Host, Operator or Unknown ownership and supply the checked artifact's content
  digest or content revision. DataStore, ValidationChain and the driver require
  every blocking diagnostic to explicitly permit Agent repair; a receipt flag or
  business error code cannot promote host-owned/unknown targets. Remove
  `ValidationChain.repairable_codes`; tool execution's correctable-code allowlist
  remains unchanged. Artifact syntax helpers leave ownership Unknown for the host
  to classify; candidate JSON/schema diagnostics explicitly classify Agent.
- Add required `RepairPolicy.max_no_progress: NonZeroU64` (default 3). Validation
  and tool repair checkpoint per-fingerprint occurrence counts, including A/B
  cycles, and stop with `VALIDATION_NO_PROGRESS` plus bounded safe codes.
  Observed artifact content/rule/owner define progress regardless of completion
  envelope or diagnostic prose. Unobserved diagnostics still bind the candidate.
  Restart and compaction preserve the counts independently of repair limits,
  budgets and deadlines. Hosts must update constructors, policies and checkpoint
  consumers; no old-format fallback. See docs/design/api.md.

- Add `ExternalResult::Failed.repair_hint: Option<String>` to the public tool
  contract (adapters must set it explicitly; no old-format fallback). Known
  correctable file failures now return safe code/hint tool feedback instead of
  ending the Run. File descriptor version is `runtrell-files-3`; numeric input
  revisions/ranges must be nonnegative, and create revision is explicitly null.
  Execution and schema repair share checkpointed counts/time limits. Settled
  failures only cease blocking finalization after their protocol feedback is
  acknowledged in the checkpoint. Recovery never replays the saved operation.
  Storage/permission/unknown errors, resource limits, retry, budget and deadline
  controls retain their stop/reconciliation behavior. Hosts must update their
  `Failed` constructors; see `docs/design/api.md` for classification details.

- Fix same-generation lease heartbeats aborting checkpoint saves with `Conflict`
  in MemoryStore and PgStore. `RunStore::renew` now extends only the owner's
  lease without advancing `RunRecord::revision`. Observe renewal via the returned
  `Claim` and persisted `owner.lease_until`, not a revision increment. Checkpoint
  CAS, ownership/generation/expiry fences, cancellation and deadlines still apply.

- Fix OpenAI Responses and chat completions (including OpenRouter) rejecting
  logical tool names such as `files.list`. Definitions and historical calls now
  use deterministic names within the provider's 64-character ASCII constraint;
  returned calls restore the original logical names. Distinct names that collide
  produce a configuration error before sending. Gemini names remain unchanged.

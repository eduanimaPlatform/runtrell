# Changelog

## 0.9.0

- Admit and rejoin child Runs within the active phase of a parent. A phase
  closes after its children finish, their work is reconciled, and their
  admission operations are settled. The next phase can reuse a logical child
  operation key with a fresh identity.
- Let tool input authorization return `Error::InvalidToolInput` for a new
  call the model can correct. The runtime checkpoints `TOOL_INPUT_INVALID`
  for that call before quoting or starting an attempt.

## 0.8.0 — media limits and read recovery

- Demote the oldest replayed tool documents and images to their existing text
  references when model input media exceed size or count limits. Keep the
  pending tool exchange and host inputs active. Token pressure still triggers
  context compaction when configured.
- Allow a recovery pass over the run's own saved file revisions after media
  demotion or compaction. Further reads of the same covered ranges count toward
  the stall limit. Document views report coverage of fixed source page ranges
  so a view-only loop can also stall.

## 0.7.0 — 2026-11-23 — session media replay

- Replay fixed tool-result documents and images in their original position in
  the active session, with current authorization on each request. Compaction
  keeps earlier media as text references and retains the pending exchange.
- Size Responses reservations using recorded media sizes and verify the complete
  input with the provider's token count before sending inference.

## 0.6.0 — 2026-11-22 — intermediate finalization

- Configure ordered phases within a Run. An intermediate submission seals its
  validated artifact, purges its transcript and fences its claim. The next phase
  opens a fresh conversation after host payload erasure is confirmed.
- Select tools and completion rules per phase. Phase changes preserve the Run's
  account, usage and attempt timestamps.
- PostgreSQL uses schema 5 for phase seals and retired operation markers. Provision
  a fresh schema for this release.

## 0.5.0 — 2026-11-22 — generic completion, typed progress and transcript purge

- Configure final submission or validated acknowledgement, typed repair and
  compaction progress, and transcript purge at successful finalization.
- Open an authorized Workspace image by logical path as a standard tool result.

## 0.4.0 — 2026-11-21 — request-sized input bounds and cancelled intent release

- The Responses adapter derives `input_upper_bound` from the prepared request,
  capped by the configured input limit, so compaction fires only as the real
  history grows instead of always or never.
- Cancelling a run releases reservations held by unsent model intents,
  including after a store reconnect.
- The continuation message tells the model to resume from the recorded progress.

## 0.3.0 — 2026-11-20 — allowance settlement and resource extension (breaking)

- Certified OpenRouter BYOK allowance evidence can settle together with the
  attempt, persisted receipt and shared balance in Memory or PostgreSQL.
- A provider adapter can separately reserve and settle a bounded lease
  extension with `DispatchRequest::extend_resource`. Direct Images SVG stays
  unavailable because its author request cannot enforce an output token cap.

## 0.2.0 — 2026-11-20 — bundled model catalog (breaking)

- Select providers with `connect_model`, a catalog ID and `Connection`.
  The bundled catalog supplies the endpoint, capabilities, limits and pricing.
- Managed sends require an outstanding reservation and use the original
  attempt's price snapshot for settlement. Use `SettlementRecovery.price: None`
  for bundled attempts.
- The connection settings include the retry policy. Documented transient
  Responses errors can create separately reserved attempts.
- OpenAI Responses connections can enforce a lower prompt or context ceiling
  with the provider's free input count before inference.
- Bundled Sunburst and Flare bindings support one bounded PNG generation or edit
  per attempt. Direct GPT-Image-2.6 uses the catalog's inherited Images 2/2.5
  policy to settle fully unreported cache counters as zero.
- GPT-6.1 Terra offers an opt-in Standard background Responses binding.
  The runtime persists its response ID and polls it against the original reservation.
- The Memory and PostgreSQL allowance ledgers support atomic, receipt-idempotent
  consumption of a certified shared provider allowance.
- OpenRouter credits and BYOK bindings require a payer-specific credential and
  match billed costs to the selected route. BYOK pins the separate upstream and
  credit charges. The FP8 credits binding prices both eligible routes before send.


## Included in 0.2.0 — text-only model file reads (breaking)

- `files.read` and `artifacts.read` return a UTF-8 `text` range to the model.
  Binary artifacts return `BINARY_ARTIFACT` with bounded metadata and guidance;
  invalid UTF-8 or binary controls are rejected. Host storage APIs still return
  raw bytes. The file descriptor version is `runtrell-files-4`.
- Stored output previews use text and trim an incomplete UTF-8 character.
  Their descriptor version is `stored-output-2`. `Tool::authorize_input` is
  called on new tool calls and saved-result replay so a revoked grant cannot
  expose a stored failure hint.

## Included in 0.2.0 — configurable no-progress stop (breaking)

- `RepairPolicy.max_no_progress` is now `Option<NonZeroU64>`: the default is
  `Some(3)`, an explicit `None` disables the repeated-failure count, and
  `Some(N)` sets another threshold. This applies to output validation and tool
  input/execution repair. Rust callers that construct the field must use the
  option type; serialized policies must explicitly include the field.
- The full repair policy is pinned in Run checkpoints. A resumed runtime with a
  different policy is refused before further model or tool work. Previously
  saved checkpoints without the field are not reopened; no migration is provided.
  Budget, deadline, cancellation and authorization gates remain in effect.

## Included in 0.2.0 — partial price evidence (breaking)

- Add `Price::cost_breakdown` and optional `Usage.price_evidence` with a pinned
  price and independently known input/output micro-USD components. Components
  round independently for audit and are not additive billing amounts. OpenAI
  Images records them even when missing cache data keeps the full bill unknown;
  SVG author cost remains in `SvgUsage`. Owner recovery recomputes components,
  preserves known evidence, and still settles the complete attempt only once.
  Rust callers constructing `Usage` must initialize `price_evidence` or use
  `..Usage::default()`; no consumer revision or stored operation is changed here.

## Included in 0.2.0 — owner commands (breaking)

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

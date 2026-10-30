# Changelog

## Unreleased

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

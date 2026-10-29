# Changelog

## Unreleased

- Add `ExternalResult::Failed.repair_hint: Option<String>` to the public tool
  contract (adapters must set it explicitly; no old-format fallback). Known
  correctable file failures now return safe code/hint tool feedback instead of
  ending the Run. File descriptor version is `runtrell-files-2`; numeric input
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

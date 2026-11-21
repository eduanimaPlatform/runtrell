# Completion and transcript retention

## Final submission

`RunSpec.completion.mode` defaults to `CompletionMode::FinalSubmission`.
The driver parses the final response, checks `completion.output_schema` when
configured, and calls the registered validator on that same candidate. The
finalizer receives the candidate and its bound validation receipt. The response
that passes validation completes the run.

`completion.provider_format` requests the configured schema on tool-free turns
when the model port advertises support. The driver always checks the configured
schema locally. JSON syntax and schema diagnostics route to a tool-free
format repair. `Validator::repair_target` can select this route for additional
format checks. Workspace repairs keep the run's authorized tools available.
A blocking diagnostic assigned to a host, an operator, or an unknown owner ends
agent repair.

## Validated acknowledgement

Set `completion.mode` to `CompletionMode::ValidatedAck` and set
`completion.validation_tool` to a trusted, offered tool. The tool returns
`ValidationFeedback` with its candidate, the run-bound `ValidationReceipt`,
the version of the state it checked, and a `RepairTarget`. Its result and the
result of `Validator::completion_basis` must describe the same state. Include
all fixed input identities and mutable output versions relevant to acceptance
in that basis. The registered tool and validator enforce access to that state.
A validation tool may also report typed feedback under final submission.

A live pass opens a short request with no tools, history or attachments. The
model acknowledges with `{"acknowledged":true}`. The driver compares the current
basis with the proof again before accepting the acknowledgement. When the basis
changes, the driver reopens validation. An already reserved request retains its
pinned body for reconciliation. The finalizer receives the original candidate
and receipt. When adopting mutable outputs, the finalizer compares the accepted
state with the version it commits, including the owner claim. The proof basis
is available via `RunRecord::validated_ack_basis()`. The success
transition remains fenced by the control store.

The configured validation tool and final validator share the same diagnostic
occurrence counters. A configured validation tool returns format repairs to
the submission surface and Workspace repairs to the authorized catalog. A
rejected validation result is counted when the paired tool answer and repair
state are checkpointed. Adapter and storage failures follow the normal
reconciliation rules.

## Progress during compaction

A trusted `Tool::progress` adapter derives observations from the pinned input
and the settled result. The standard Workspace tools report actual read intervals
and completed writes, edits and deletes. Overlapping reads on the same file
revision are merged. A write of previously seen content at the same path
does not advance the progress counter. A first validation pass for a new
basis and candidate advances it. The checkpoint persists this accounting with
the protocol cursor.

`RunSpec.max_repeated_reads` stops after the configured number of consecutive
completed reads that add no coverage. `CompactionPolicy.max_without_progress`
allows that many completed compactions at the same progress point. The run stops if it needs another compaction before new progress. Both are optional. Hosts can match
`Terminal::Stalled(ProgressStop::RepeatedRead | ProgressStop::Compaction)`
and `ProgressStop::Validation` directly.

`CompactionPolicy.preserve_progress` projects read coverage, completed
Workspace changes and outstanding diagnostics. Successful write arguments and
provider envelopes are omitted from the summary model request. The latest
paired tool exchange remains available in the normal model request. The
original history and results remain recoverable until the transcript is purged.

## Purge on successful finalization

Set `RunSpec.transcript = TranscriptPolicy::PurgeOnSuccess` to remove the run's
persisted input, messages, media references, tool arguments/results, final
submission, validation bodies and compaction summaries. The finalizer must
adopt any required output before the success transaction. Memory and PostgreSQL
commit the purge and the success marker together. PostgreSQL also redacts late
observations and removes reclaimed execution records inside that transaction.
Run identity, terminal state, completion ID, fingerprints for idempotent
retries, the accounting ledger, settlement, usage and timestamps stay
available. A custom ControlStore advertises support through
`RunStore::supports_transcript_purge`. Retained identifiers contain host
metadata only.

The store admits purge after all attempts have settled and no execution needs
its recovery data. Unknown, failed, and cancelled runs follow their regular
recovery and retention lifecycle. A read after successful purge exposes
`RunRecord::transcript_purged()` and metadata; the host reads its adopted output
from its own store. Child admission stores the original request fingerprint,
so the parent ledger has no extra copy of the child's input.

Workspace files, staged document or artifact outputs, host exports and provider
retention have separate ownership. Hosts can use the [Run erasure](erasure.md)
workflow to delete run-owned Workspace and artifacts. Physical media, WAL,
backups and provider archives follow the host's storage retention policy.

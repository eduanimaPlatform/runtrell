# Chat host lifecycle

This example uses the existing Run, session, transaction, operation and queue
contracts. It adds no runtime protocol, schema migration or conversation daemon.
The host's message table is the conversation source of truth; Run checkpoints
and operation receipts remain execution/recovery state, not another chat archive.

| Concern | Public Runtrell | Chat host (including shared adapters) | Connection transport | Deployment |
|---|---|---|---|---|
| Conversation body | Session stores sequence/context revision only; checkpoint belongs to one Run | Owns durable user/assistant messages, summaries and authorized references | Delivers input/output; no ownership of history | Supplies durable storage |
| Admission | `start_in` atomically admits Run and queue entry; tenant/principal/request key detects replay/conflict | Rechecks current identity, permissions/configuration; saves stable message ID and input with admission in one transaction | Retries same ID; reports pending/conflict explicitly | Restarts shared workers |
| Ordering | Session sequence, predecessor wait, context CAS; stale Run fails `CONTEXT_STALE` | Serializes message admission; reconstructs current history; retains rejected/stale input for explicit resolution | Must not report stale input as success or silently discard it | No conversation-specific thread required |
| Wait/wake/retry | Machine waits, durable queue/operations, fenced claims and reconciliation | Discovers queued/unfinished work even when notifications are lost; schedules retries under existing contracts | Wake notifications are hints; disconnect is not cancel | Supplies shared queue workers/availability |
| Reply completion | Stores terminal output/receipt/completion; normal `drive` return releases active Run slot | Publishes saved output idempotently to message; task returns; later read uses saved result | Reconnect reads saved result, no new inference | May close pool/process; scaling is separate |
| Lease / slot | Claim records owner generation/expiry; lease expiry alone does not reclaim permits; normal drive returns slot | Renews active work; reclaims/reconciles interrupted work; does not poll empty drives between messages | Does not own claim lifetime | Worker shutdown/recovery policy |
| DB connection | Bounded store transactions/read calls borrow pool connections | Owns shared pool and closes it on process shutdown, not per conversation | No pinned conversation connection | DB/pool availability; DB need not scale to zero |
| Provider request/state | Call-scoped request, settlement/recovery; opaque envelopes bound to Run/session | Rebuilds new Run input from safe messages/summary/references; never copies old envelope to new Run | Dropping HTTP/SSE/WebSocket is not external cancellation | Networking and process lifetime |
| Temporary process | Existing execution cleanup receipts distinguish stop/reclaim/unknown | Starts sandbox/Python only if needed; saves Workspace/Artifacts independently | Future drop proves neither stop nor cleanup | Owns provisioned compute; no idle TTL imposed here |
| Duplicate / concurrent / stopped input | Same spec/key converges; changed spec conflicts; session checks fence stale work | Message uniqueness plus transaction; recover after admission or saved reply before publication; explicit stale diagnostic | Stable ID across retries; explicit cancellation only | Can replace worker after lease expiry |

Session is not an OS thread, Tokio task, WebSocket/SSE connection, provider
conversation ID or sandbox. `Waiting` means a machine wait, not waiting for the
next chat message. A completed Run stays terminal. A new input starts a new Run;
an interrupted reply resumes its original Run/checkpoint.

## Public demonstration and limits

`cargo run -p runtrell-postgres --locked --example chat_host` requires
`RUNTRELL_TEST_DATABASE_URL` pointing at a disposable PostgreSQL database. It
creates isolated UUID namespaces using the existing test convention. No provider
credentials, remote inference, tools, sandbox or UI are used. History is passed as
host-constructed JSON text, not as imported checkpoints/provider envelopes.

The example runs separate child processes for A, B, admission-only, recovery and
contract checks. Each closes its pool before exiting; the parent holds no pool.
The contract test invokes the same phases in separate processes. Assertions cover
history restored into B's actual model request, zero idle calls/slots, duplicate
IDs, concurrent session ordering/stale rejection, saved-result publication,
duplicate wake and expired-worker fencing, and a detached subscriber while a
reply continues. Fake token/cost reservations demonstrate attempt accounting;
they do not measure real provider behavior.

The minimal host deliberately surfaces a stale input with its original message
and Run intact. It does not implement a product policy for automatically rebasing
that input; a host can explicitly rebuild history and admit a new request key
using the existing API. No public API change is required for this demonstration.
An outdated context revision at admission returns `Conflict` and rolls back the
host transaction. An already admitted Run that becomes stale fails with
`CONTEXT_STALE` before new model/tool work; neither case silently rebases history.
Production authorization, retention, transport integration, cleanup after an
abrupt in-flight process death, and compute scale-to-zero are not demonstrated.
Existing provider HTTP tests (`run_session_continuation` for each adapter) reject
cross-Run/session envelopes before send; the chat example never imports them.

See [state](state.md), [PostgreSQL transactions](postgres.md), and
[provider binding/recovery](providers.md) for the authoritative runtime contracts.

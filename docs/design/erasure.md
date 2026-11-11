# Run erasure

A trusted host authorizes the RunHandle before calling `PgStore::erase_run`.
The operation erases the run and its descendants, including every agent namespace
in the supplied `RunPayloadStore`. The host supplies **all** run-owned backends;
a partial adapter must never report success. Independent source material owned by
another run or service is outside this ownership boundary. Hosts must also remove
their business records and any exported copies through their own deletion workflow.

## Admission and order

1. Lock the root then discover and lock descendants under their parent locks.
   Every run must be terminal, have no live owner lease, no pending/unknown stop,
   cleanup or settlement, no unsettled/unknown/accepted operation and no unreclaimed
   execution. Otherwise return `Pending` without erasing anything. A cancelled run
   still requires normal settlement and physical cleanup; deletion is not refund
   or a substitute for Unknown recovery.
2. In one synchronous PostgreSQL transaction, persist the `pending` erasure marker
   and remove database bodies. All normal Run reads and transitions reject marked
   runs with `Closed`, including original-request retries, claim, checkpoint and
   replay. Late-result and execution writers take the Run lock as well, preventing
   stale writers from repopulating erased data. Discovery and child creation use
   the same parent locks. Preserve parent identity for retry discovery.
3. Invoke the host's idempotent payload eraser for every discovered run. DataStore
   uses one atomic run envelope for all agent namespaces. Its durable tombstone
   removes workspace revisions, staged data, blobs/artifacts, grants, validation
   evidence, completions and context, and fences all future reads and writes.
4. Only after every payload backend acknowledges deletion, mark the whole tree
   `done` in a second database transaction. Success is idempotent.

## Removed and retained

Remove input/images, original request copies, checkpoints/transcripts/messages,
validation/output/delivery bodies, cancellation/error text, tool/model input and
output, recovery proofs and transport evidence, late results, and reclaimed
execution inventory (programs, arguments, observations and resource paths).

Retain only terminal body-free Run tombstones, request identity (tenant,
principal, request key), parent/account/session identity and sequence for integrity
and deduplication; accounting attempts retain operation identity/kind, reserve,
quantities, price, usage/cost counters, settlement/recovery audit and timing.
Budget accounts and attempt attribution remain unchanged; `budget_entries` remains
queryable. Owner receipts retain their accounting identifiers and normalized
numeric evidence. Opaque `svg_usage.reported` JSON retains only its model identity
and unsigned numeric counters in results, observations, recovery evidence and
owner settlement receipts. Malformed fields become non-content invalid markers;
mismatched numeric evidence remains mismatched. Unknown evidence never becomes
measured zero or a valid bill through deletion. The
receipt trigger permits only that exact deterministic redaction while the owning
Run is fenced `pending`; other receipt updates and deletion remain prohibited.
All retained identifiers (including owner command keys/digests, settlement basis,
model IDs and quantity names) are trusted non-content identifiers by contract:
hosts must never put answers, prompts or results in identifier fields. Erasure does
not reinterpret these fields as a place for payload text.

## Failure and recovery

A failed first transaction rolls back both marker and database scrubbing. Once
`pending` commits, the run cannot execute again, even if external deletion fails
or the process dies. Retry the same RunHandle with the same complete backend set;
retry walks the retained tree, repeats payload deletion and then marks completion.
There is no undo/resurrection endpoint. Concurrent erasers are safe because both
payload erasure and final markers are idempotent. Existing owner receipts are
historical evidence only: returning an old resume receipt never resumes a marked
run. New resume commands fail `Closed`.

This is logical deletion from current runtime storage. PostgreSQL MVCC dead tuples,
WAL, replicas/backups, filesystem snapshots and storage-provider version history
require the host's storage retention/backup policy; SQL deletion is not a physical
media-sanitization claim. External NamespaceBackend adapters must replace the
complete opaque run envelope atomically, prevent ABA, and honor the same durable
removal policy for their own retained versions.

The DataStore persistence unit is now a versioned Run envelope. Local storage
initializes a layout marker in an empty root and refuses unmarked nonempty roots.
External adapters must configure a fresh isolated key prefix for this layout;
they must not point the new implementation at an old namespace-keyed store.
There is no implicit old-format reader, migration or deletion of preexisting
assets. Owners must establish the new layout and stop old runtime writers before
enabling erasure; unrelated historical data needs its own authorized disposition.

Updating a host binary without changing an existing nonempty, unmarked local root
fails at `DataStore::local` with `Error::Storage`, before the host can serve work.
For adoption, stop every writer sharing the old root, provision a new empty
directory or volume, and configure every new host replica to use that same root.
Let DataStore create its marker; adding a marker to old data does not convert the
layout. Retain the old root separately until its authorized disposition is decided.
Old Run/artifact references cannot be resumed against the empty store: retire the
corresponding disposable test fixtures and create fresh coherent fixtures through
the host's supported workflow. This change supplies no old-data migration or
automatic reset of either host records or runtime accounting.

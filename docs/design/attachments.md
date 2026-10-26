# Optional attachments and page selection

`ModelRequest.attachments` fixes each external file's opaque reference, version,
page range, MIME, size and SHA-256 before estimation, reservation and operation
recording. It also accepts the existing `ImageReference` Workspace/Artifact
fixed-version contract for documents; no second storage reference format exists.
Authorized stored bytes are prepared once with `ImageModel`, before estimation.
Images selected by `DocumentView` use the existing `image_inputs` path.

Tool attachment references remain in messages and operation receipts for history,
provenance and recovery. Only tool messages after the last final assistant message
contribute binary inputs to the next request (initial Run images remain explicit
host inputs). Selection happens before preparation; estimation and sending share
the same `PreparedModelRequest`. Compaction retains older references as text,
without reactivating their binary inputs. Reopen a saved fixed-version attachment
with the same tool when needed again; original-file acquisition is unnecessary.
Partial assistant responses from `max_tokens` continuations do not close the tool
exchange: document and image selection stays unchanged across repeated continuations
and checkpoint recovery, until a final assistant response closes the exchange.
Provider count/byte limits on a tool exchange produce repairable tool feedback
asking for a narrower range, without deleting the saved result or raising limits.

`AttachmentResolver::authorize` checks current trusted Run/operation/destination
authorization without acquiring content or issuing URLs. `resolve` may perform
host RPCs and returns transport information for the exact declared target.
Bytes are measured and hashed locally. URL metadata is an owner/resolver
attestation of the fixed version: Runtrell does not independently fetch or verify
the URL target. Rotating a URL changes no logical request or persistent digest.
Changing content, version or range requires a new prepared request and estimate.

Adapters with no result-query capability return Unknown from lookup without
resolving attachments, issuing URLs, fetching bytes or resending inference.
Stored results require current authorization but no original-file acquisition.
An adapter-observed resolution failure before inference is a known unsent failure
with zero inference usage; loss after sending is Unknown and retains the reservation.
A crash or outer cancellation without a durable unsent receipt remains unknown.
Resolver RPCs
are distinct from potentially billable provider inference HTTP requests.

`document.view` accepts an authorized attachment and an optional inclusive,
one-based page range. For PDF, without a range it returns the page count only.
With a range it saves a PDF subset as an Artifact, returning `attachments` for the next
model request. It uses lopdf, rejects encrypted/malformed PDFs and out-of-range
pages, and bounds input/output bytes and selected pages. The
[document view transformation](document-view.md) retains visible annotations and
form appearances/resources while disabling active content; XFA is rejected.
PNG/JPEG/WebP use existing still-image validation and `image_inputs` (host MIME
allowlist applies); without a range a whole image is attached, and only range
1..1 is accepted. An external resolver must supply bytes for this local tool
(a URL is not downloaded by Runtrell).
Tool results retain source reference and range as provenance. Access grants and
Policy govern source access and saved-result retrieval. No business role names
or task-specific execution engine are part of this feature.

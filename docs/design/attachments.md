# Optional attachments and page selection

`ModelRequest.attachments` fixes each external file's opaque reference, version,
page range, MIME, size and SHA-256 before estimation, reservation and operation
recording. It also accepts the existing `ImageReference` Workspace/Artifact
fixed-version contract for documents; no second storage reference format exists.
Authorized stored bytes are prepared once with `ImageModel`, before estimation.
Images selected by `DocumentView` use the existing `image_inputs` path.

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
one-based page range. Without a range it returns the page count only. With a
range it saves a PDF subset as an Artifact, returning `attachments` for the next
model request. It uses lopdf, rejects encrypted/malformed PDFs and out-of-range
pages, and bounds input/output bytes and selected pages. PNG uses existing image
validation and `image_inputs`; other formats are rejected. An external resolver
must supply bytes for this local tool (a URL is not downloaded by Runtrell).
Tool results retain source reference and range as provenance. Access grants and
Policy govern source access and saved-result retrieval. No business role names
or OCR-specific execution engine are part of this feature.

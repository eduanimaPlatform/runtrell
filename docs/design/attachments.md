# Optional attachments and page selection

`ModelRequest.attachments` fixes each external file's opaque reference, version,
page range, MIME, size and SHA-256 before estimation, reservation and operation
recording. It also accepts the existing `ImageReference` Workspace/Artifact
fixed-version contract for documents; no second storage reference format exists.
Authorized stored bytes are prepared once with `ImageModel`, before estimation.
Images selected by `DocumentView` use the existing `image_inputs` path.

Tool results retain their fixed media references in the transcript and operation
receipts. Requests replay active media after the batch of tool results that
introduced them. The batch follows tool result order and introduces each fixed
reference once. Initial input media is placed with the first user message.
Provider adapters build the prompt from those fixed positions, so an appended
turn leaves the preceding input items unchanged when the media transport is stable.
Stored inputs and external byte inputs use the same bytes on each send. A rotating
external URL can change the wire prefix while preserving the fixed target.

Each replay prepares or resolves the exact fixed version and checks current
authorization. Compaction summarizes older tool results and preserves their fixed
references as text. The pending exchange remains replayable. The existing
transcript purge and phase transition also apply to media references.

Count and byte limits apply to the complete model request. Before preparation
and token estimation, the driver removes the oldest replayed media from the
active input until the request fits. The original tool output, text references
and order remain intact. Retained media keep their positions, and reopening a
demoted reference introduces it at the new tool exchange. The checkpoint and
saved model request record the replay boundary for recovery. Host input images
and the pending exchange stay active. An exchange that cannot fit with the host
inputs receives repairable tool feedback and keeps the original receipts.
This works with or without a compaction policy. With a policy, the driver
checks token pressure against the request after media demotion.

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
With a range it saves a PDF subset as an Artifact and returns `attachments` for
model history. Reopening the same fixed selection with the same content reuses
its stored reference. It uses lopdf, rejects encrypted or malformed PDFs and
out-of-range pages, and bounds input/output bytes and selected pages. The
[document view transformation](document-view.md) retains visible annotations and
form appearances/resources while disabling active content; XFA is rejected.
PNG/JPEG/WebP use existing still-image validation and `image_inputs` (host MIME
allowlist applies); without a range a whole image is attached, and only range
1..1 is accepted. An external resolver must supply bytes for this local tool
(a URL is not downloaded by Runtrell).
Tool results retain source reference and range as provenance. Access grants and
Policy govern source access and saved-result retrieval. No business role names
or task-specific execution engine are part of this feature.

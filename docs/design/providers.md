# Optional provider adapters

`runtrell-providers` has no default features. `openai`, `gemini`, and `openrouter`
select Responses, Developer API generateContent, and chat completions respectively.
Each implements `ModelPort`; the driver contains no provider selection.
The host supplies the complete endpoint URL, credential, model, limits, price,
model capabilities, request/response byte bounds, input token upper bound, Run,
and optional session. No discovery, credential loading, or price catalog is provided.

Public OSS image support is separately provided by `ImagePort` adapters:

| Provider / adapter | Feature | Image input | Generation | Reference edit | Mask edit |
|---|---|---|---|---|---|
| OpenAI / OpenAiImages | openai | Authorized references | Host-declared | Host-declared | Unsupported |
| Gemini / GeminiImages | gemini | Authorized references | Host-declared | Host-declared | Unsupported |

These connect to standard `ImageGenerate`/`ImageEdit` using the same `ImageTool`.
No provider fallback is implicit. OpenAi ModelPort alone does not implement image
generation. Image MIME, byte/count/dimension and transport bounds come from the
host's ImageLimits; PNG is the default, still JPEG/WebP are opt-in. See
[images](images.md) for the OpenAI JSON wire and exact recovery/settlement behavior.

## Contract

Optional file attachments use `ModelRequest.attachments`: external declarations
fix opaque reference ID, version, page range, MIME, byte length and SHA-256.
Stored attachments reuse the existing Workspace/Artifact `ImageReference` contract.
External declarations contain no locator or
payload. The host fixes these before quoting/reservation (it may compute the
digest while acquiring the original before constructing the declaration).
`PreparedModelRequest` exposes the same immutable declarations and can own already
authorized stored bytes, prepared through `ImageModel` before estimation.
`Config.attachment_resolver.resolve` is called only at invoke,
with the trusted ToolContext and provider endpoint. The host must
authorize that Run/principal/operation, fixed version and destination on each call.
It must not log payloads or return locators in reference IDs or other saved fields.

Inline bytes are hashed and measured by Runtrell; URL results carry host-attested
digest and length of the immutable target. The host must verify those against
the fixed source when minting the URL; Runtrell does not fetch that URL. An
attestation is a trust boundary, not independent proof of remote content.
Mismatch rejects before provider inference HTTP; resolver authorization/resolution
RPCs may already have occurred. Resolution errors have no payload-bearing message.
Resolved inputs have redacted Debug and no Serialize implementation. They live
in prepared memory or during wire construction; no raw input is copied into operation,
checkpoint or continuation state. Lookup calls only the separate metadata-only
`authorize` hook, never `resolve`: Unknown and reservation retention remain
unchanged after a lost response. Saved-result recovery does not acquire originals.
Before inference, an adapter-observed resolution failure produces `ATTACHMENT_NOT_SENT` with known zero
inference usage; after sending, lost responses remain Unknown with reservation held.
This does not account for any independent host resolver RPC charge.
Crash/cancellation without a saved unsent result conservatively retains Unknown.

Document capabilities declare PDF and image support separately for URL and inline
bytes. OpenAI Responses maps PDFs to input_file (file_url/file_data), PNG/JPEG/
WebP to input_image. Gemini maps PDFs and those images to fileData/inlineData.
OpenRouter document inputs are unsupported even if host flags are enabled.
Host model flags can narrow these paths. Unsupported MIME, empty/invalid
declarations and unsupported paths reject before sending. The fixed host token
ceiling must cover all declared documents, including PDF pages, and is unchanged
by resolution. Declared bytes bound inline Base64; URLs have a fixed 8192-byte
bound. Count, total declared bytes and worst-case JSON wire size are checked
before reservation. No request options permit late document attachment.
The host also validates the actual file format and PDF page count against its
model/token ceiling. These document limits do not use the ordinary (PNG by default)
Workspace image inspection limits. `DocumentView` supports bounded PDF page
selection and PNG/JPEG/WebP via existing image input; see [attachments](attachments.md).
All three model adapters accept host-enabled JPEG/WebP through ordinary image
input. OpenRouter rejects the separate document attachment path before sending.
`ModelPort::check_input_attachments` checks tool-exchange count/byte limits
without resolving data; the driver returns repairable feedback for an oversized
tool result, preserving its complete operation receipt and textual references.
Provider echoes of a resolved URL, full UTF-8 payload
or Base64 payload are redacted before saving reply/continuation metadata; this
does not replace the host's authorization and source-access policy.

Adapters are bound to a RunHandle and session at construction. Provider messages
contain a versioned, target-bound state envelope; every envelope and the invoking
Run are checked before any send. Persist reply metadata as a `provider` Message
(the runtime already does this). Response IDs are saved inside this binding;
Responses uses store=false and replays output items, including encrypted reasoning,
without previous_response_id or a remote conversation. Gemini signed parts and chat
reasoning details are replayed from
the envelopes. Synthetic assistant history following an envelope is skipped to
avoid duplicating the saved response. A compacted history without envelopes starts
statelessly. Hosts must preserve the trusted Run/session binding on reconstruction;
these envelopes are saved host state, not an untrusted import or authorization token.
Arbitrary request metadata is never forwarded as wire options.

`RequestOptions` in request.metadata.provider_options supplies JSON schema.
Image inputs use `ModelRequest.images` with Workspace/Artifact references and
`ImageModel` for authorized resolution into a `PreparedModelRequest` before
estimation. Raw inline-image options have been removed. `OpenAiImages` and
`GeminiImages` supply generation/reference editing through `ImagePort`; see
[images](images.md) for limits, authorization, storage and settlement.
StopReason distinguishes
completed, max_tokens (continuable truncation), content_filter, tool_call and
unknown. ModelReply.complete is false for filtered/unknown stops so the stock
driver cannot accept them as candidates. A fully received max_tokens reply remains
complete with an explicit truncation reason; it does not claim semantic completion.
Truncated/filtered/unknown responses never expose executable tool calls. Stop reason
is saved in metadata. The ordinary driver settles a max_tokens response, appends
the host-configured continuation instruction, and re-quotes a new operation.
Partial calls are neither executed nor replayed as complete calls. Budget,
deadline and optional host continuation limits govern further sends. Automatic
compaction uses the host port's compaction_limits and routes summary requests by
model; active continuation state and original history remain Run-bound.

Usage uses the runtime Usage type: cached/image input are subsets of input;
reasoning/image output are subsets of output; cached image input is their input
intersection. Missing counters remain None, including missing subsets. Metadata
separates absent, partial and totals-reported usage. Gemini candidates and thoughts
are disjoint and are summed once into output only when both are reported (or the
host explicitly declares that reasoning cannot occur). All other totals already
include their subsets. Model identity is the returned identity when available;
prices are never inferred. With a host price matching the returned model identity,
micro_usd is calculated by the runtime Price.cost (one rounding, subset-aware);
missing price-sensitive counters leave cost unreported. No price or a model
mismatch also leaves cost unreported. The runtime rejects mismatched priced model
identity; hosts must pin/configure the model whose usage they intend to settle.

HTTP redirects and automatic retries are disabled; one invocation sends at most
once. Connection/body/JSON failure after starting send is Unknown, never proof of
absence. No lookup or idempotency guarantee is implemented: lookup always Unknown,
recovery returns None, and the default attempt policy is one attempt. Explicit HTTP
errors carry a sanitized classification, status and Retry-After; provider bodies,
messages and credentials are not propagated. Recognizable spending/balance codes
are separate from transient rate limits; ambiguous quota errors remain rate limits
or unclassified. Retry-After delta seconds and HTTP dates are converted to the
runtime delay; raw headers are exposed only through the typed failure result.

Capabilities contain host-overridable model limits, schema/tool/parallel support,
separate input/generation/reference-edit/mask-edit declarations and counter
availability, with fixed subset semantics. The adapter's
lookup/idempotency support remains none regardless of model settings. Input bound
is a host supplied conservative token ceiling, never a guessed tokenizer count.
Bounds, unsupported options and mismatched model/state are rejected before send.

Tests use loopback fake HTTP only. No live-provider or hosted acceptance is claimed.
This implementation is independently written from the public wire contracts; no
Rig provider code is extracted and the adoption manifest remains unchanged.

## Wire references

- [OpenAI function calling](https://developers.openai.com/api/docs/guides/function-calling)
- [OpenAI file inputs](https://developers.openai.com/api/docs/guides/file-inputs)
- [OpenAI Images generation](https://developers.openai.com/api/reference/resources/images/methods/generate)
- [OpenAI Images reference edits](https://developers.openai.com/api/reference/resources/images/methods/edit)
- [Gemini document inputs](https://ai.google.dev/gemini-api/docs/document-processing)
- [Gemini generateContent](https://ai.google.dev/api/generate-content)
- [Gemini native image generation and editing](https://ai.google.dev/gemini-api/docs/image-generation)
- [OpenRouter error contract source](https://github.com/OpenRouterTeam/docs/blob/main/api_reference/errors-and-debugging.mdx)

The endpoints are supplied as full URLs by the host, so generateContent's model
path is also host-owned. Gemini function-call parallelism has no request switch
here; a model configured without parallel support returning multiple calls is
rejected as an unknown stop, and none of those calls is executable.

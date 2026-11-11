# Optional provider adapters

`runtrell-providers` has no default features. `openai`, `gemini`, and `openrouter`
select Responses, Developer API generateContent, and chat completions respectively.
Each implements `ModelPort`; the driver contains no provider selection.
The host supplies the complete endpoint URL, credential, model, limits, price,
model capabilities, request/response byte bounds, input token upper bound, Run,
and optional session. No discovery, credential loading, or price catalog is provided.
`ModelLimits.timeout` is the HTTP timeout in milliseconds, and `ToolContext.deadline`
is an absolute Unix-millisecond deadline compared with the wall clock; the request
timeout is the smaller of the two remaining durations.

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

OpenAI Responses and chat completions (including OpenRouter) use a per-request
tool-name table for definitions, ordinary function-call history, and returned
calls. Nonempty names of at most 64 ASCII letters, digits, underscores or hyphens
are unchanged; other logical names use the 64 lowercase hexadecimal characters
of `digest(JSON string of the logical name)` (SHA-256). The table includes logical
names from tools and ordinary history. Any collision between distinct logical
names is a configuration error before sending, rather than a guessed alias.
Names are independent of catalog order. Returned names must match the table and
the current tool catalog; unknown names produce the existing unknown-stop reply.
Host requests, catalog identities, call IDs and replay proofs retain logical
names. Saved provider continuation content retains wire names for exact replay;
the deterministic mapping also applies after adapter reconstruction. Gemini
generateContent keeps logical names unchanged.

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
once. Connection/body/JSON failure after starting send remains an unknown result,
never proof of absence. The result carries `ProviderDiagnostic` with a fixed phase
(`pre_send`, `send`, `response_read`, `response_decode`, `http_error`), send certainty,
and the actual HTTP status when received. `send` has unknown certainty even when a
local peer observed bytes; `response_read`, `response_decode`, and `http_error` have
`sent` certainty. Attachment resolution failure is `pre_send` with
`definitely_not_sent`. This evidence does not imply known usage or permission to
retry. No lookup or idempotency guarantee is implemented: lookup always Unknown,
recovery returns None, and the default attempt policy is one attempt.

The public `ExternalResult::provider_diagnostic()` and persisted
`Operation::provider_diagnostic()` expose typed evidence. Failed image responses
carry it through `ImageResponse` into the operation, including when usage is too
incomplete to settle and the outward result becomes Unknown. The diagnostic's
HTTP status is the actual response status; the existing serialized `Failure.status`
remains the effective status used for classification and retry. The only copied
provider identifiers are `error.type`, `error.code`, `error.param` for OpenAI and
OpenRouter; Gemini uses `error.status`, `error.code`, and the first
`google.rpc.BadRequest` field violation's `field`. Each identifier is omitted
unless the entire value is 1–128 bytes and uses only ASCII letters, digits,
underscore, period, hyphen, and square brackets; invalid
values are not truncated. The Gemini field shape follows the
[Google BadRequest schema](https://developers.google.com/data-manager/api/reference/rest/v1/BadRequest).
Provider messages, headers, URLs, and bodies are never copied into diagnostics.

Explicit HTTP errors retain sanitized classification, status and Retry-After.
Recognizable spending/balance codes
are separate from transient rate limits; ambiguous quota errors remain rate limits
or unclassified. Retry-After delta seconds and HTTP dates are converted to the
runtime delay in milliseconds (`ExternalResult::Failed.retry_after`; seconds are
multiplied with overflow checked). `ProviderDiagnostic` contains no header values;
the existing serialized `Failure.retry_after` retains the validated Retry-After value.

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

## OpenAI Images usage

`OpenAiImages` does not reuse the Responses usage parser. `/images/generations` and
`/images/edits` return the `ImagesUsage` object, so `openai_images_usage::normalize` maps only
what that schema and the vendor's pricing note state.

Sources, read 2026-11-03: the OpenAI OpenAPI description
(`github.com/openai/openai-openapi`, `main` at c2aecc2, 2026-10-31), schema `ImagesUsage`; and
the [image generation guide](https://developers.openai.com/api/docs/guides/image-generation),
section "Cached input pricing".

| Response field | Runtime counter |
|---|---|
| `input_tokens` (= `input_tokens_details.text_tokens` + `image_tokens`) | `input` |
| `input_tokens_details.image_tokens` | `image_input`; `text_tokens` goes to `quantities.text_input_tokens` |
| `output_tokens` ("image tokens in the output image"), or `output_tokens_details.image_tokens` when present | `output`, `image_output`; `output_tokens_details.text_tokens` goes to `quantities.text_output_tokens` |
| `total_tokens` | checked against `input_tokens + output_tokens`, not stored |
| (none; the schema has no reasoning category but does not say none is billed) | `reasoning` stays `None`; a price whose reasoning rate differs from the output rate therefore refuses the cost (`IMAGE_USAGE_PRICE_COUNTER_UNKNOWN`), and hosts set both rates equal for image models |

`output_tokens_details` is not in `ImagesUsage` but real responses carry it; it is accepted only
when consistent with `output_tokens`.

**Cached counters.** For direct Images API calls, the 2026-11-11 user decision applies the
GPT Image 2/2.5 billing rule to the two exact 2.6 IDs, `gpt-image-2.6-sunburst` and
`gpt-image-2.6-flare`. The [pricing page](https://developers.openai.com/api/docs/pricing)
lists identical Standard token rates for 2.5 and 2.6 Sunburst/Flare: text input $5,
image input $8, and image output $30 per million tokens, with cached rates $1.25 and $2.
The [guide's cached-input statement](https://developers.openai.com/api/docs/guides/image-generation#cached-input-pricing)
explicitly excludes cache discounts for direct Images API requests, including edits, for 2 and
2.5 only; applying that exclusion to 2.6 is the user's policy decision, not an official 2.6
statement. Responses API image tools and other routes can have different cache behavior and
are outside this rule.

For the eight exact IDs in `openai_images_usage::GPT_IMAGE_2_FAMILY`, missing `cached` and
`cached_image_input` are filled with zero, and all input tokens are billed at ordinary rates.
Explicit nonzero cache counters contradict this rule and cause `IMAGE_USAGE_INCONSISTENT`;
reported counters remain visible for diagnosis. Owner recovery applies the same rule to stored
Images usage, filling only missing cache counters and refusing contradictory values. Other
models retain unknown missing cache counters. `Price::cost` keeps its refusal semantics and
the price is the host's, with all four input rates independent (see [budget](budget.md)).
For an unpriced or incomplete response, `Usage.price_evidence` can retain a known
component and the host-priced `SvgUsage` can retain the author component.
The host reads this evidence through saved `Operation` usage and the owner
recovery audit; it is informational, not a partial charge.
If a response omits one total, valid counters on the other axis can still
produce that axis's evidence. Contradictory reported parts produce no price
evidence; the complete amount remains unknown in either case.
An image that arrived with unsettleable usage is kept; the adapter returns the counters it can
prove and `Usage.unknown_reason`:

| `unknown_reason` | Meaning |
|---|---|
| `IMAGE_USAGE_ABSENT` | no `usage` object |
| `IMAGE_USAGE_MALFORMED` | a counter is not a non-negative integer |
| `IMAGE_USAGE_INCOMPLETE` | a schema-required counter is missing |
| `IMAGE_USAGE_INCONSISTENT` | parts do not add up to the totals |
| `IMAGE_USAGE_CACHED_UNREPORTED` | model outside the exact direct Images no-cache rule has unreported cache counters |
| `IMAGE_USAGE_MODEL_UNPRICED` | the response names a model other than the requested one or a declared alias |
| `IMAGE_USAGE_NO_PRICE` | the host configured no price |
| `IMAGE_USAGE_PRICE_COUNTER_UNKNOWN` | another counter that affects the bill is unknown |
| `IMAGE_USAGE_COST_OVERFLOW` | the bill does not fit a `u64` of micro-USD |

The response carries no model field in the published schema, so an absent model means the
requested model. `ImageCapabilities.response_model_aliases` lists exact response model names the
host declares to bill as the requested model; usage is then recorded under the priced model.
Any other reported model is not priced.

SVG author usage is a separate `Usage.svg_usage` component; complete raw JSON is
retained even if malformed or the returned model is unknown. Its counters and
price never enter image counters or image-model `Price::cost`. Exact author rates
come from the host's `ImageCapabilities.svg_author_prices`, including verified
reasoning/output inclusion semantics. Missing/malformed/inconsistent/unpriced
author usage prevents aggregate settlement; it is never a free component.
The component stores its applied rate snapshot; `Usage::amount` adds image and
author costs once, while `total_tokens` and `reasoning_tokens` are not extra lines.
The 2.6 Standard test catalog records 5 / 1.25 / 8 / 2 / 30 USD/M tokens
(text / cached text / image / cached image / output), E1/E5 accessed 2026-11-05.
It supplies no fabricated author-model rate or live billing claim.

# Optional provider adapters

`runtrell-providers` has no default features. `openai`, `gemini`, and `openrouter`
select Responses, Developer API generateContent, and chat completions respectively.
Each implements `ModelPort`; the driver contains no provider selection.
Use `connect_model` with a canonical `provider/model-name`, an optional
binding name, and a `Connection`. The bundled catalog supplies the endpoint,
wire model, capabilities, limits and pricing. Connection supplies a credential
resolver, Run/session, timeout, reasoning setting and optional lower limits.
`Connection::input_token_cap` bounds the billed prompt.
`Connection::context_cap` bounds the sum of the prompt and allowed output.
With a lower context, reservation subtracts the maximum allowed output to
compute the effective prompt ceiling. Without lower limits, the adapter
reserves against the published prompt ceiling.

For OpenAI Responses, a persisted reservation authorizes the adapter to send
its final input to the free `POST /v1/responses/input_tokens` endpoint with
the same key and deadline. This includes the input after attachment resolution,
function definitions and the structured output schema. A count above the
ceiling, a count failure or an unusable reply prevents inference. Uncapped
connections use the published bound and skip counting. Other executable
families can use a lower context when their published prompt bound fits the
configured context together with the allowed output.
[Responses input counts](https://developers.openai.com/api/reference/resources/responses/subresources/input_tokens/methods/count) ·
[OpenAI counting and billing](https://help.openai.com/en/articles/4936856-understanding-and-counting-tokens).

Resolution rejects unknown IDs, disabled features and unverified bindings
before acquiring credentials.
The host also pins the model retry policy (`max_attempts`, `backoff`, `max_elapsed`,
all durations in milliseconds). Its default is 3 attempts including the first,
1,000 ms initial backoff and 60,000 ms total elapsed time (`RetryPolicy::default`).
Zero policy values are refused at adapter construction. The host selects finite
operational bounds and includes this policy
in its saved configuration identity and supplies the same value on reconstruction.
`ModelLimits.timeout` is the HTTP timeout in milliseconds, and `ToolContext.deadline`
is an absolute Unix-millisecond deadline compared with the wall clock; the request
timeout is the smaller of the two remaining durations.

## Explicit transient HTTP failures

OpenAI Responses retries only a received `429` with `error.type=rate_limit_error`
and `error.code=slow_down`, or a received `503` with
`error.type=service_unavailable_error` and `error.code=server_is_overloaded`.
[OpenAI Rate limits](https://developers.openai.com/api/docs/guides/rate-limits#handle-rapid-traffic-increases-and-model-overload)
documents these conditions and instructs callers to honor `Retry-After`. The
adapter checks the actual HTTP status and body, not a JSON effective status or
header alone. Billing, spend, quota and input errors do not enable retries.
Other model adapters and error codes remain nonretryable until their official
provider contract is documented here. A response lost after sending, including
a stream interrupted after output, remains Unknown and is never automatically
resent. Each model adapter sends at most once per runtime attempt.

On a documented transient failure the adapter saves a `Failed` result and
Provider diagnostic. Its retry marker is a nonnegative millisecond delay; `0`
means no `Retry-After` was supplied, so the driver applies bounded exponential
backoff and jitter. With a header, the driver waits at least its full delay.
The driver creates a fresh attempt and reservation after the wait; it never
settles an unknown previous attempt at zero. A failed attempt's unknown cost
and operation slot remain held and count against the ancestor budgets and
capacity. Retry needs the host's `continue_with_unknown` policy and available
budget/capacity; otherwise it stops without sending. Attempts and total elapsed
time, including in-flight retry sends, remain bounded across worker restarts, as do the original Run deadline and
shared account. A header longer than the remaining time is not shortened to
force an early send. A present but unprocessable `Retry-After`, including a
numeric value too large for the duration type, also cannot authorize an early
send. If the retry window expires after reservation but before sending, the
driver settles that provably never-sent attempt at measured zero, keeps its
failed no-send audit record, and releases only its reservation and slot. Sent
or observed attempts cannot use that resolution.

Owner recovery of an unsettled `Failed` attempt uses the same documented,
evidence-bound `SettlementRecovery` path as an `Applied` attempt; it preserves
the failed result, error and diagnostic. Merely knowing the HTTP status or a
bill amount that does not establish the missing counters cannot settle it.
Settlement never resumes a Run. See [budget](budget.md#price-lines-and-owner-settlement-recovery).

The provider-neutral image tools and wire fixtures exercise storage, authorization
and single settlement. See [image contracts](images.md).

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
`Connection.attachment_resolver.resolve` is called only at invoke,
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

Catalog capabilities pin permitted document transports and MIME types. The
bundled Responses and GenerateContent bindings use authorized inline PDF and
image bytes. Count, total declared bytes and worst-case JSON wire size are
checked before reservation. `DocumentView` supports bounded PDF page selection
and image inputs through [attachments](attachments.md).
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

Usage preserves provider token totals, cache and reasoning subsets, model
identity, and the measured cache-write TTL split. Each attempt saves the
catalog revision, digest, route, rule and exact price basis. Responses sends
Standard requests with an explicit cache TTL and applies the measured prompt
band to the full attempt. Gemini output combines candidate and thinking
counters. Missing price-sensitive counters or incompatible route evidence keep
the reservation pending.

HTTP redirects and automatic retries are disabled; one invocation sends at most
once. Connection/body/JSON failure after starting send remains an unknown result,
never proof of absence. The result carries `ProviderDiagnostic` with a fixed phase
(`pre_send`, `send`, `response_read`, `response_decode`, `http_error`), send certainty,
and the actual HTTP status when received. `send` has unknown certainty even when a
local peer observed bytes; `response_read`, `response_decode`, and `http_error` have
`sent` certainty. Attachment resolution failure is `pre_send` with
`definitely_not_sent`. This evidence does not imply known usage or permission to
retry. No lookup or idempotency guarantee is implemented: lookup always Unknown,
recovery returns None, and only the documented explicit Responses errors are eligible for retries.

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
runtime delay in milliseconds only for documented retryable failures
(`ExternalResult::Failed.retry_after`; seconds are multiplied with overflow
checked, and an unrepresentable accepted header prevents an early retry).
`ProviderDiagnostic` contains no header values;
the existing serialized `Failure.retry_after` retains the validated Retry-After value.

The driver reserves the maximum price before recording the send intent.
The adapter consumes the single-send authorization before resolving the
credential, after constructing and checking the wire payload. The ledger
recomputes the bill with the attempt's pinned snapshot. Replaying that receipt
does not charge a second time.

The opt-in `standard-background` binding for GPT-6.1 Terra sends
`background: true` with the default completion window and reserves the
full Standard request before submitting. OpenAI temporarily stores the
response for polling. The driver stores the accepted response ID with its
attempt. Reconciliation retrieves that ID on the same binding without
submitting again. Polling errors keep the accepted ID and reservation.
This binding accepts text and function calls with text results. Attachments
and images are rejected during preflight.
[OpenAI background mode](https://developers.openai.com/api/docs/guides/background).

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

The catalog supplies the endpoints and `generateContent` model path. The Gemini
2.5 Flash-Lite binding sets `thinkingBudget: 0` and checks that usage contains no
positive thought count. The Developer API limits access to users who previously
used the 2.5 family. The 3.5 Flash-Lite `generateContent` binding remains
documented. Its positive thinking needs an enforceable chargeable-token ceiling.
The Interactions API exposes a separate hard cutoff contract. The 3.1 Flash
Lite Image price lists 1,120 output tokens per 1K image. The public model limit
is 4,096 output tokens. The GenerateContent contract limits candidate tokens
and documents thought tokens as a separate count. The image bindings remain
documented until the wire provides a combined bound or separate enforced
thinking and image counts.
[Gemini 3.1 Flash Lite Image model card](https://ai.google.dev/gemini-api/docs/models/gemini-3.1-flash-lite-image) ·
[Gemini image pricing](https://ai.google.dev/gemini-api/docs/pricing#gemini-3.1-flash-lite-image).

[Gemini Developer API lifecycle](https://aistudio.google.com/docs/deprecations#gemini-2.5-flash-models) ·
[Gemini thinking limits](https://aistudio.google.com/docs/thinking#token-limits).

## OpenAI Images usage and reservation

Sunburst and Flare have direct PNG generation and edit bindings. The adapter
fixes `n=1` and uses the nonstreaming API with no partial images. It sends
an explicit size and quality. Callers supply the size. Quality defaults to
`low`. The catalog reserves from the public 2.6 calculator values below.

| Size | Low | Medium | High |
| --- | ---: | ---: | ---: |
| `1024x1024` | 823 | 1,449 | 1,756 |
| `1536x1024` | 664 | 1,132 | 1,372 |
| `1024x1536` | 664 | 1,132 | 1,372 |

The calculator reports estimated output tokens and excludes reference inputs
and prompt tokens. OpenAI publishes no 2.6 hard token limit for these cells.
The binding reserves twice the calculator estimate for the chosen cell. This
100% margin is a Runtrell reservation policy. The prompt is limited to 4,096
UTF-8 bytes and reserves 8,192 text tokens, including headroom for provider
formatting. Edit allows one bounded PNG reference with each edge at most 1,536
pixels. The reservation uses 14,048 image input tokens for that reference, twice
the calculator's 7,024-token square `max` estimate as a reference budget proxy.
The legacy input tiling examples provide a separate scale comparison and the
current vision calculator excludes GPT Image edits. The reference budget is
an estimate with headroom. It has no provider token enforcement. Reservation
uses undiscounted rates. An actual amount above the saved reservation triggers
the managed overrun policy.
[OpenAI 2.6 calculator](https://developers.openai.com/api/docs/guides/image-generation#calculating-costs) ·
[OpenAI image inputs](https://developers.openai.com/api/docs/guides/images-vision#gpt-image-model-inputs) ·
[OpenAI image generation](https://developers.openai.com/api/reference/resources/images/methods/generate) ·
[OpenAI image edit](https://developers.openai.com/api/reference/resources/images/methods/edit).

The 2.6 direct raster bindings declare
`inherited_direct_image_2_2_5_zero_when_unreported`. This pricing policy
inherits the direct Images 2/2.5 structure: when neither cache counter is
reported, both count as zero. Reported counters take precedence. A positive
cache total on an edit still needs the image subset to price the discount.
The attempt's saved pricing snapshot carries the policy used for settlement.
[OpenAI cached input pricing](https://developers.openai.com/api/docs/guides/image-generation#cached-input-pricing).

The direct SVG operation is unavailable before credential resolution. The
Images API accepts an author model and reasoning effort but has no field
for the author request's `max_output_tokens`. An executable SVG binding
needs that cap on the author wire and must reserve the bounded prompt and
reference using the author model's catalog rates and cache rules.
[SVG request and usage](https://developers.openai.com/api/reference/resources/images/methods/generate) ·
[SVG billing](https://developers.openai.com/api/docs/guides/image-generation#cost-and-latency).

## OpenRouter payer and routes

`deepinfra-standard` requires `ApiKey::openrouter(key, OpenRouterPayer::Credits)`.
The host configures a workspace in which BYOK keys cannot serve the binding.
`deepinfra-byok` requires `OpenRouterPayer::Byok`. The upstream workspace key
must use the published paid SKU and have shared-capacity fallback disabled.
The adapter checks the credential payer against the catalog binding before
HTTP. It verifies the returned provider, `usage.is_byok`, `usage.cost` and
`usage.cost_details.upstream_inference_cost` for the selected SKU. Evidence is
normalized to micro-USD and carried with the original snapshot.

BYOK pins three profiles. The upstream profile supplies the provider cost.
OpenRouter's list-price reference supplies the basis for a maximum 5% platform
fee, charged through a separate router profile. Reservation assumes none of
the monthly allowance is available. Settlement checks the upstream cost
against the upstream profile and uses the reported credit fee within the
allowance-independent ceiling, so zero and partially discounted fees settle.

`fp8-standard` is a credits binding with DeepInfra and Cloudflare candidates.
The request supplies both exact tags through `provider.only` and disables
fallback, which lets OpenRouter choose one ranked endpoint. The attempt stores
both price snapshots, reserves the maximum of the reachable charges, and
settles against the one reported provider. The binding uses the 32,000-token
published shared prompt ceiling. Missing or inconsistent route or payer proof
keeps the reservation.
[OpenRouter routing controls](https://openrouter.ai/docs/guides/routing/provider-selection) ·
[OpenRouter request accounting](https://openrouter.ai/docs/cookbook/administration/usage-accounting) ·
[OpenRouter BYOK](https://openrouter.ai/docs/guides/overview/auth/byok) ·
[Endpoint SKUs](https://openrouter.ai/api/v1/models/meta-llama/llama-3.1-8b-instruct/endpoints) ·
[DeepInfra FP8 SKU](https://deepinfra.com/meta-llama/Meta-Llama-3.1-8B-Instruct-Turbo).

## Catalog API families and billing meters

The schema admits the API families below. Executable bindings appear as
`verified` in the generated [catalog](../../catalog/README.md). Each additional
wire tuple requires a positive registry entry, enforceable admission limits,
complete meter evidence, a price graph for the SKU, and fixtures for the
successful and rejected requests. The executable families are
`openai_responses`, `openai_images` (PNG only), `gemini_generate` (2.5
Flash-Lite completion), and `openrouter_chat` (listed Llama FP8 bindings).
The remaining families are schema entries and cannot open a provider
connection. The GPT-6 Responses registry accepts models on that wire contract
through the catalog. Models with different wire or metering rules need a
separate registry entry.

| Family | Wire and billing meters |
| --- | --- |
| `openai_responses`, `openai_chat`, `openai_batch` | Completion and Batch item tokens. Distinguish uncached input, cache reads, supported write TTLs and output including reasoning. Provider Tool SKUs may add search/file calls, tool-result tokens, or container starts and duration. [Responses](https://developers.openai.com/api/reference/resources/responses/methods/create) · [Batch](https://developers.openai.com/api/docs/guides/batch) · [Tool SKUs](https://developers.openai.com/api/docs/pricing#tools) |
| `openai_images`, `openai_audio`, `openai_realtime`, `openai_live` | Direct raster and authoring tokens, streaming partial images, speech characters or tokens, and transcription media or tokens. Realtime meters use terminal modality details. Live duration has active and standby SKUs, plus separately priced downstream models and Tools where applicable. [Images](https://developers.openai.com/api/docs/guides/image-generation) · [Audio](https://developers.openai.com/api/docs/guides/audio) · [Live](https://developers.openai.com/api/docs/models/gpt-live-1) |
| `gemini_generate`, `gemini_interactions`, `gemini_live`, `gemini_batch`, `vertex_generate`, `vertex_live`, `vertex_batch` | Developer and Cloud text or multimodal generation, Live and Batch. Account for input modality, cache, candidates, thoughts and Tool prompts. Grounding SKUs distinguish a grounded request from the actual billable search queries. [GenerateContent](https://aistudio.google.com/api-reference/generate-content) · [Interactions](https://aistudio.google.com/api-reference/interactions-api/create-interaction) · [Developer SKUs](https://aistudio.google.com/docs/pricing) · [Cloud SKUs](https://cloud.google.com/model-garden/pricing) |
| `vertex_predict`, `vertex_lro`, `gemini_cache`, `vertex_cache` | Imagen image count, Veo video duration and Lyria clip count. Long-running submit, poll and download share the provider job identity. Cache storage uses token count multiplied by preserved time and is tied to a generation and resource ID. [Cloud SKUs](https://cloud.google.com/model-garden/pricing) · [Developer cache](https://aistudio.google.com/api-reference/caching) · [Vertex cache](https://docs.cloud.google.com/gemini-enterprise-agent-platform/reference/rest/v1/projects.locations.cachedContents) |
| `anthropic_messages`, `anthropic_batches` | Input and output tokens, cache read and 5-minute or 1-hour creation. Batch bills per item. Web Search meters eligible calls and model tokens. Code Execution meters the applicable minimum time per container, plus paid duration beyond allowances. [Cache](https://platform.claude.com/docs/en/build-with-claude/prompt-caching) · [Batch](https://platform.claude.com/docs/en/build-with-claude/batch-processing) · [SKUs](https://platform.claude.com/docs/en/about-claude/pricing) |
| `openrouter_chat`, `openrouter_responses`, `openrouter_batch`, `openrouter_images`, `openrouter_audio`, `openrouter_video` | Routed completion and Batch item tokens, requests and Tool calls. Image, speech, transcription and video SKUs use the selected endpoint's token, image, pixel, character or media-duration meters. Bind each cost to its generation ID, effective endpoint and payer. [API families](https://openrouter.ai/support/) · [Batch](https://openrouter.ai/blog/announcements/batch-api/) · [Billing evidence](https://openrouter.ai/docs/cookbook/administration/usage-accounting) |
| `xai_chat`, `xai_responses`, `xai_batch`, `xai_images`, `xai_video`, `xai_audio`, `xai_voice` | Completion and Batch tokens, host Tool calls, X Search post/profile records, image counts, video duration and audio characters or duration. Voice sessions meter elapsed audio under the applicable VAD rules and eligible text-input events. [SKUs](https://docs.x.ai/developers/pricing) · [Speech-to-speech](https://docs.x.ai/developers/models/speech-to-speech) · [Video](https://docs.x.ai/developers/model-capabilities/video/overview) |
| `mistral_chat`, `mistral_batch`, `mistral_ocr`, `mistral_audio` | Completion and Batch tokens, OCR pages and eligible cached pages, and audio duration, character or token meters per speech SKU. [SKUs](https://docs.mistral.ai/inference/pricing?currency=usd) · [OCR](https://docs.mistral.ai/api/endpoint/ocr) · [Speech](https://docs.mistral.ai/studio/audio/text-to-speech/generate-speech) |
| `deepseek_chat`, `deepseek_responses`, `deepseek_messages`, `deepseek_speech`, `deepseek_tools` | Completion input hit/miss and output tokens, TTS weighted characters, STT seconds rounded up, eligible search/fetch items and OCR/parse pages. A completion with native search may bill result context tokens even when the call fee is zero. [SKUs](https://api-docs.deepseek.com/quick_start/pricing/) · [TTS](https://api-docs.deepseek.com/guides/tts/#billing) · [ASR](https://api-docs.deepseek.com/guides/asr/#billing) |
| `cohere_chat`, `cohere_transcribe` | Completion billed input/output units and applicable Tool context. Transcription uses the published evaluation plan and the evidence for the binding's actual speech SKU. [Chat](https://docs.cohere.com/v2/reference/chat) · [Transcribe](https://docs.cohere.com/reference/create-audio-transcription) · [Plans](https://docs.cohere.com/docs/transcribe) |

# Optional image capabilities

`runtrell::images` defines provider-neutral resolution and ordinary image tools.
`runtrell-providers` has no default features; `OpenAiImages` uses `openai` and
`GeminiImages` uses `gemini`. These are optional public OSS adapters.
Hosts choose catalog IDs, credentials and access restrictions.
The [bundled catalog](../../catalog/README.md) lists the executable OpenAI
raster bindings and the documented Gemini image bindings. Wire and receipt
tests use loopback transport fixtures.

## Inputs and requests

`ImageReference` wraps `ArtifactRef`, or `FileVersion` plus `Namespace` and MIME.
`RunSpec.images` pins initial inputs. Tools return `image_inputs` containing these
same references. The driver includes them in normal model history using
[fixed-reference replay](attachments.md).
Initial message metadata, checkpoints and operation inputs store references and
hashes, never transport Base64. `ImageResolver` checks Policy, current Run,
namespace, owner/explicit grant, revision, actual length, SHA-256 and image structure.
It reads bounded chunks through WorkspaceStore/ArtifactStore. A known ID, parent
relationship or forged hash is not authority. Cross-Run sharing requires the
existing store's explicit, revision-bound grant.

All images are resolved before `ModelPort::input_upper_bound` and reservation.
`PreparedModelRequest` has no mutable request or attachment setters; only
`with_max_output` adjusts the output cap. Hosts use
`ImageModel { inner, resolver, runs }` with Runtime's RunStore and Policy.
The wrapper rechecks storage access and Policy before send and retrieval.
Reconstruction resolves the pinned references again and requires the same hash.
Text-only ModelPorts reject attachments with Unsupported. Provider adapters
independently check capability, image and transport bounds. The provider-bound input allowance covers the complete request, including images.

## Tools and recovery

Connect `ImageTool { provider, resolver, runs, clock, output_access, config }`
through `ToolSet::image`, then select `ImageGenerate`, `ImageEdit` or `SvgGenerate`.
The three closed public request types are `RasterGenerate`, `RasterEdit` and
`SvgGenerate`. All accept prompt, optional size/quality/complexity/background;
only raster edit accepts references/mask. Raster formats are PNG (default), JPEG,
WebP; SVG generation accepts only SVG (default). There is no SVG edit request.
Generate rejects references/masks; raster edit requires references and rejects SVG inputs.
GeminiImages uses native generateContent text/inlineData,
`responseModalities: [TEXT, IMAGE]` and one candidate. Input, generation,
reference edit and mask edit are separate capability declarations. Mask support
is rejected at construction and preflight; remote lookup/idempotency are absent.

The verified OpenAI bindings use the direct generations and edits endpoints.
The JSON wire uses the catalog model, prompt, explicit size and quality, `n: 1`
and `output_format: "png"`. Edit supplies one validated inline PNG in
`images: [{ image_url: "data:image/png;base64,..." }]`. Callers select one of
the three listed sizes. Quality defaults to `low` and also accepts `medium`
and `high`.
Complexity and background are sent if supplied. The catalog reservation defines
the prompt and reference image limits. [OpenAI Images bounds](providers.md#openai-images-usage-and-reservation).
Unsupported request options detected before send return `InvalidToolInput`
with the field and its permitted values or an instruction to omit it. Model
tool calls receive the usual `TOOL_INPUT_INVALID` repair without a send.
A separate `ImageSizePolicy` applies to each enabled model action
(generation, reference edit, SVG generation) through `ImageCapabilities`.
`Values` lists exact `WIDTHxHEIGHT` strings; `Dimensions` declares multiple,
pixel, edge and aspect bounds. `auto` also permits omission; `Unsized` permits
only omission for endpoints without a size option. Missing declarations fail
closed before reservation or send. Tool schemas show exact choices or the
dimension rule. For `Values`, schema diagnostics list every allowed choice when
the list fits the bounded hint. For `Dimensions`, rejected caller sizes become
typed `InvalidToolInput`; the runtime sends `TOOL_INPUT_INVALID` with `/size`
and a repair guide to the Agent. The same typed path covers tighter
`ImageLimits` payload bounds. Sizes
are passed through after validation against the selected action and endpoint.
Exactly one `data[0].b64_json` is required. Both adapters reject mask editing.
GeminiImages rejects size/non-PNG/options rather than silently ignoring them.
Output MIME must be enabled by the selected operation.

SVG generation additionally requires `ImageCapabilities.svg_generation` and
`image/svg+xml` in allowed MIME. The wire fixes `n:1`, `output_format:"svg"` and
uses generations. Stream, partial images, compression and prompt rewriter are
absent from the closed request. Optional `svg.reasoning.effort` is independent
of complexity. The entire `svg` object requires `svg_model_selection`, and its
`model` is required when supplied. Without access, omit the object. Reasoning
effort is optional/null (author default), or none/minimal/low/medium/high/xhigh/max;
the selected author's own supported efforts still apply. Credentials, transport, timeout, authorization,
receipt binding, artifact storage and settlement use the ordinary image path.

OpenAI usage maps reported totals and image subsets without inference. Reported
text details are retained as `text_input_tokens`/`text_output_tokens` quantities;
missing details stay absent except under an explicit catalog rule. The 2.6
direct raster rule treats wholly unreported cache counters as zero. Invalid
image responses retain parsed usage for the
ordinary billed-failure settlement path. No adapter owns a second ledger.
`Usage.svg_usage` preserves the complete `svg_usage` JSON plus separate typed
author model/input/output/reasoning/total counters. It never populates image
counters. Missing/malformed author evidence or an unlisted author rate keeps
the author cost unknown and the reservation held, including on HTTP failures.
The SVG metering fixtures use an exact `SvgAuthorPrice` to declare that reasoning is already
included in output. The applied rate snapshot is saved with the author usage.
`Usage.micro_usd` and `Price::cost` cover the image component; `Usage::amount`
adds the author component once. `Usage::total` sums each component's input+output,
never adding either reported total or reasoning again. Duplicate catalog entries
are ambiguous and remain unpriced. See [provider usage](providers.md#openai-images-usage-and-reservation).

`Tool::quote` checks and resolves the entire input before reservation. The tool
reserves the larger of the adapter's quote and its configured reserve. Runtime owns the single
operation/reservation/settlement; the ImagePort writes no ledger. RegisteredTool
and configuration wrappers preserve preflight and result authorization.
Image tools already return bounded references; applying their `stored_output`
setting is rejected during ToolSet assembly because it hides those typed inputs.
An executable SVG binding must include the image and authoring charges in its
pre-send reservation. The author call needs a wire-enforced
`max_output_tokens` from the request and a bound for the author prompt and
reference. The catalog applies the author's own price and cache rules. Direct
Images SVG is unavailable because its wire has no author output cap.

Validated results become immutable Intermediate Artifacts under stable IDs bound
to Run, operation, attempt, request, adapter/config identity, limits and namespace.
Raster `image_inputs` feed subsequent model turns and `image.edit`. SVG references
are returned separately in `svg_outputs`, with empty `image_inputs`, so XML is not
silently forwarded into raster/model input. Both are authorized on lookup/replay.
An owner-only
Checkpoint staging record stores result and usage under the same binding before
return. Lookup reads this record and Artifacts, reauthorizes and verifies hashes,
and never invokes the Provider. Missing/expired records return Unknown, never a
NotFound proof allowing another paid send. Hosts retain staging receipts for at
least the recovery window and do not purge them during active recovery.
Successful ledger replay runs `Tool::authorize_result` before exposing results.

Known usage with invalid Base64, MIME, byte count, image container structure, dimensions,
output count or incomplete output is a billed Failed result and settles normally.
OpenAI Images records zero usage for a complete HTTP 4xx structured rejection
without reported usage or image data, and for confirmed pre-send failures.
When the provider identifies a correctable image input field, the settled
failure returns a tool error with a field-specific hint. Received usage takes
precedence. Missing usage on a 5xx or success response and ambiguous sends,
including post-send timeouts and truncated responses, retain reservations.
Input/output image tokens retain subset semantics.
`image_input_count`/`image_output_count` record image counts.
Failed attempts retain sanitized provider diagnostics in the operation ledger
for reconciliation. Preflight validation rejects unsupported or invalid inputs
before send. Each image attempt sends at most once.

## Bounds and authorization

| Setting | Default |
|---|---:|
| Each image | 8 MiB |
| SVG source (`max_svg_bytes`) | 8 MiB |
| Images per request/result | 4 |
| JSON request/response including Base64 | 48 MiB each |
| Width / height | 8192 each |
| Pixels | 16,777,216 |
| MIME | image/png |
| Prompt | 65,536 UTF-8 bytes |

Limits require `ceil(max_enabled_payload_bytes / 3) * 4 * count + 65,536` to fit both transport
ceilings. Both adapters reject transport ceilings below their image limits. PNG
inspection checks signature, chunk structure/CRC, IHDR fields, dimensions, IDAT
and final IEND. Still JPEG (baseline/extended/progressive DCT marker/scan framing)
and WebP (RIFF bounds, VP8/VP8L and optional VP8X dimensions) can be enabled by
adding `image/jpeg`/`image/webp` to `allowed_mime`; PNG remains the default.
Inspection does not decompress pixels or judge image quality; hosts can add a
separate validator for that purpose. Animated WebP is rejected. Raster explicit `size`
must satisfy both the host's model/action policy and the payload bounds. Timeouts
are host-configured and bounded by operation/Run deadlines, never fixed at two seconds.

SVG uses bounded Base64 → UTF-8 XML bytes, without a raster decoder or pixel/dimension
inspection. XML must have one SVG-namespaced root, balanced tags and no DTD.
The exact decoded bytes are stored and hashed: no reserialization or normalization.
This is transport validation, **not host sanitization or browser publication approval**.
Hosts own static SVG acceptance, external-resource/script restrictions and display/PDF policy.
SVG request size, when present, must be `auto` or positive `WIDTHxHEIGHT`; SVG output
does not use raster pixel bounds.

Policy permissions: `image.resolve`, `image.send`, `image.retrieve`, `image.save`.
Reference targets: `image:<reference-digest>`. Tool call permissions:
`image.generate`/`image.edit`/`svg.generate`; saved result targets: `image-result:<binding>`.
Policy is paired with trusted Access objects and cannot replace store grants.

## Acceptance scope

`providers/tests/http.rs` runs separately named OpenAI and Gemini suites using
fake HTTP generation/edit wire, usage, errors,
unsupported/denied requests, saved lookup and ordinary image-inclusive driver
requests. `testkit::image_contracts` runs generation → Artifact → model input →
reference edit, saved-response loss/recovery, one send per paid operation, exact
account totals, hash/foreign access rejection, billed failures and unknown usage
against Memory and PostgreSQL control stores. Real providers, application
integration and pixel/semantic quality are outside this acceptance.
The Pg image suites also reopen independent pools and local Artifact stores,
inspect settlement row counts and exercise the image HTTP adapter through Runtime.
OpenAI-specific cases cover its exact inherited JSON fields, all three output
formats, malformed single-image responses with retained usage, paid failures,
absent/partial usage and post-send disconnect reservation retention in real Pg.
OpenAI acceptance is based on the OpenAI suites; Gemini results are separate evidence.
Images 2.6 fixtures test each action's wire and omitted options, no-send rejections,
exact SVG source/digest/replay, independent author rates and unknown costs, cache
preservation and no double counting. Standard price fixtures record E1/E5 and
the 2026-11-05 access date. No real provider call or host publication acceptance
is implied by these mocks.

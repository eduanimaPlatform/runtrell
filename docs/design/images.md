# Optional image capabilities

`runtrell::images` defines provider-neutral resolution and ordinary image tools.
`runtrell-providers` has no default features; `OpenAiImages` uses `openai` and
`GeminiImages` uses `gemini`. These are optional public OSS adapters.
Hosts choose models, endpoints, credentials, prices, token ceilings, roles and
access restrictions. Images never require a sandbox or interpreter.

| Public OSS provider | Adapter / feature | Raster generation | Raster reference editing | SVG generation | Mask editing |
|---|---|---|---|---|---|
| OpenAI | OpenAiImages / openai | Host-declared | Host-declared | Host-declared | Unsupported |
| Gemini | GeminiImages / gemini | Host-declared | Host-declared | Unsupported | Unsupported |

## Inputs and requests

`ImageReference` wraps `ArtifactRef`, or `FileVersion` plus `Namespace` and MIME.
`RunSpec.images` pins initial inputs. Tools return `image_inputs` containing these
same references; the ordinary driver selects the latest tool exchange for the
next model input using the [attachment window](attachments.md). Earlier references
remain available as history and can be reopened with the same tool.
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
independently check capability, image and transport bounds. The host token ceiling
must cover the complete permitted image-inclusive input; tokenization is not guessed.

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

OpenAiImages uses the host's full `/v1/images/generations` endpoint, replacing
`generations` with `edits` on the same origin for reference editing. The JSON wire
is `model` (host supplied), `prompt`, `n: 1`, optional nonempty `size`, and
`output_format` (default `png`, also `jpeg`/`webp`). Reference edits add
`images: [{ image_url: "data:<mime>;base64,..." }]`. For the configured 2.6 Sunburst
or Flare model, optional quality (`auto/low/medium/high`), complexity
(`low/medium/high`) and background (`auto/opaque/transparent`) are sent only when
provided. No default complexity is sent. Transparent raster output requires PNG/WebP.
Unknown fields, `xhigh/max` quality, markup, GIF and SVG edit are rejected before HTTP.
The host declares a separate `ImageSizePolicy` for each enabled model action
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
are not rounded or replaced. The host supplies constraints verified for the
selected model and endpoint.
Exactly one `data[0].b64_json` is required. Both adapters reject mask editing.
GeminiImages rejects size/non-PNG/options rather than silently ignoring them.
Output MIME must be enabled in the host's ImageLimits.

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
missing details stay absent. Invalid image responses retain parsed usage for the
ordinary billed-failure settlement path. No adapter owns a second ledger.
`Usage.svg_usage` preserves the complete `svg_usage` JSON plus separate typed
author model/input/output/reasoning/total counters. It never populates image
counters. Missing/malformed author evidence or an unlisted author rate keeps
the author cost unknown and the reservation held, including on HTTP failures.
An exact host-verified `SvgAuthorPrice` must declare that reasoning is already
included in output. The applied rate snapshot is saved with the author usage.
`Usage.micro_usd` and `Price::cost` cover the image component; `Usage::amount`
adds the author component once. `Usage::total` sums each component's input+output,
never adding either reported total or reasoning again. Duplicate catalog entries
are ambiguous and remain unpriced. See [provider usage](providers.md#openai-images-usage).

`Tool::quote` checks and resolves the entire input before reservation. Each adapter
estimates using the host token ceiling, output cap and Price. The tool reserves
the larger of this estimate and its configured reserve. Runtime owns the single
operation/reservation/settlement; the ImagePort writes no ledger. RegisteredTool
and configuration wrappers preserve preflight and result authorization.
Image tools already return bounded references; applying their `stored_output`
setting is rejected during ToolSet assembly because it hides those typed inputs.
The automatic quote is for image rendering; hosts must include an authoring
allowance in their configured reserve. It is not an exact bound on SVG authoring.

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
Missing usage and lost/truncated HTTP responses retain reservations. Saved images
with unknown usage do not release reservations. Input/output image tokens retain
subset semantics; `image_input_count`/`image_output_count` record wire image counts.
HTTP errors use existing sanitized Failure classifications; bodies and credentials
are excluded. Unsupported, PermissionDenied and Invalid (`IMAGE_*` codes) fail
before send. There is one attempt, no hidden retry, redirect or paid reconstruction.

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

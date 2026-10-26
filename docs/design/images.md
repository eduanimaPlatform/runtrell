# Optional image capabilities

`runtrell::images` defines provider-neutral resolution and ordinary image tools.
`runtrell-providers` has no default features; `OpenAiImages` uses `openai` and
`GeminiImages` uses `gemini`. These are optional public OSS adapters.
Hosts choose models, endpoints, credentials, prices, token ceilings, roles and
access restrictions. Images never require a sandbox or interpreter.

| Public OSS provider | Adapter / feature | Generation | Reference editing | Mask editing |
|---|---|---|---|---|
| OpenAI | OpenAiImages / openai | Host-declared | Host-declared | Unsupported |
| Gemini | GeminiImages / gemini | Host-declared | Host-declared | Unsupported |

## Inputs and requests

`ImageReference` wraps `ArtifactRef`, or `FileVersion` plus `Namespace` and MIME.
`RunSpec.images` pins initial inputs. Tools return `image_inputs` containing these
same references; the ordinary driver collects them for subsequent model turns.
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
through `ToolSet::image`, then select `ImageGenerate`/`ImageEdit`. Input is
`{ prompt, references?, mask?, size?, output_format? }`. Generate rejects references/masks; edit requires
references. GeminiImages uses native generateContent text/inlineData,
`responseModalities: [TEXT, IMAGE]` and one candidate. Input, generation,
reference edit and mask edit are separate capability declarations. Mask support
is rejected at construction and preflight; remote lookup/idempotency are absent.

OpenAiImages uses the host's full `/v1/images/generations` endpoint, replacing
`generations` with `edits` on the same origin for reference editing. The JSON wire
is `model` (host supplied), `prompt`, `n: 1`, optional nonempty `size`, and
`output_format` (default `png`, also `jpeg`/`webp`). Reference edits add
`images: [{ image_url: "data:<mime>;base64,..." }]`. No quality or other implicit
options are sent. Exactly one `data[0].b64_json` is required. Both adapters reject
mask editing before send; OpenAiImages deliberately exposes generation and
reference editing only. GeminiImages rejects size/non-PNG output options rather
than silently ignoring them. Output MIME must be enabled in the host's ImageLimits.

OpenAI usage maps reported totals and image subsets without inference. Reported
text details are retained as `text_input_tokens`/`text_output_tokens` quantities;
missing details stay absent. Invalid image responses retain parsed usage for the
ordinary billed-failure settlement path. No adapter owns a second ledger.

`Tool::quote` checks and resolves the entire input before reservation. Each adapter
estimates using the host token ceiling, output cap and Price. The tool reserves
the larger of this estimate and its configured reserve. Runtime owns the single
operation/reservation/settlement; the ImagePort writes no ledger. RegisteredTool
and configuration wrappers preserve preflight and result authorization.
Image tools already return bounded references; applying their `stored_output`
setting is rejected during ToolSet assembly because it hides those typed inputs.

Validated results become immutable Intermediate Artifacts under stable IDs bound
to Run, operation, attempt, request, adapter/config identity, limits and namespace.
Returned `image_inputs` feed subsequent model turns and `image.edit`. An owner-only
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
| Images per request/result | 4 |
| JSON request/response including Base64 | 48 MiB each |
| Width / height | 8192 each |
| Pixels | 16,777,216 |
| MIME | image/png |
| Prompt | 65,536 UTF-8 bytes |

Limits require `ceil(image_bytes / 3) * 4 * count + 65,536` to fit both transport
ceilings. Both adapters reject transport ceilings below their image limits. PNG
inspection checks signature, chunk structure/CRC, IHDR fields, dimensions, IDAT
and final IEND. Still JPEG (baseline/extended/progressive DCT marker/scan framing)
and WebP (RIFF bounds, VP8/VP8L and optional VP8X dimensions) can be enabled by
adding `image/jpeg`/`image/webp` to `allowed_mime`; PNG remains the default.
Inspection does not decompress pixels or judge image quality; hosts can add a
separate validator for that purpose. Animated WebP is rejected. Explicit `size`
is `auto` or `WIDTHxHEIGHT` within the same dimension/pixel bounds. Timeouts
are host-configured and bounded by operation/Run deadlines, never fixed at two seconds.

Policy permissions: `image.resolve`, `image.send`, `image.retrieve`, `image.save`.
Reference targets: `image:<reference-digest>`. Tool call permissions:
`image.generate`/`image.edit`; saved result targets: `image-result:<binding>`.
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

# Document views and visible PDF content

The [attachment contract](attachments.md) defines authorization, fixed versions,
the active tool exchange and saved-result recovery. `DocumentView` uses the
existing `lopdf` parser and storage references; it does not render or transcribe
documents. Image inputs reuse the [existing image inspector](images.md).

Page selection retains `Annots`, normal/state appearance streams (`AP`), markup,
Ink and Stamp content, and widget values. It retains the selected widgets' field
ancestors and AcroForm `DR`, `DA`, `NeedAppearances` for rendering inherited
values. Other pages' fields are pruned. Existing appearances are preserved
instead of flattened: this avoids introducing a renderer, synthesizing fonts or
changing annotation placement. No new appearance is synthesized; XFA is rejected
because silently dropping dynamic form content would lose visible information.
Optional-content configuration is retained for layer visibility.

Actions (`A`, `AA`, `OpenAction`), JavaScript, URI/Launch payloads, embedded-file
references and multimedia activation are disabled before pruning unreachable
objects. Action entry points are removed from semantic PDF dictionaries;
arbitrary resource/appearance names such as `A` and `AA` remain valid names.
Visible annotation data and active behavior are separate concerns. This is a
bounded view transformation, not a general-purpose PDF security certification.

PDF without a range only reports page count. PNG/JPEG/WebP without a range attach
the single inspected image; an explicit image range must be `1..1`. MIME opt-in,
dimensions and byte limits come from the existing `ImageLimits`. OpenAI, Gemini
and OpenRouter support this ordinary image route when host-enabled; OpenRouter
rejects the separate document route. No additional image decoder is used.

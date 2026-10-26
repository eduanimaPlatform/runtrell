// SPDX-License-Identifier: Apache-2.0
//! Optional page inspection/selection. Original data is never returned as tool JSON.
use crate::{attachments::*, data::*, images::*, *};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

// Keep visible annotations and their appearance resources. Form field trees
// are restricted to widgets on selected pages so discarded pages cannot leak
// through AcroForm/Kids/Parent references.
fn selected_form_field(
    doc: &mut lopdf::Document,
    field: &lopdf::Object,
    selected: &std::collections::BTreeSet<lopdf::ObjectId>,
    depth: usize,
) -> Result<Option<lopdf::Object>> {
    if depth > 64 {
        return Err(Error::Invalid("DOCUMENT_FORM_DEPTH".into()));
    }
    let id = field.as_reference().ok();
    let object = match id {
        Some(id) => doc.get_object(id),
        None => Ok(field),
    }
    .map_err(|_| Error::Invalid("DOCUMENT_FORM".into()))?;
    let mut dict = object
        .as_dict()
        .map_err(|_| Error::Invalid("DOCUMENT_FORM".into()))?
        .clone();
    dict.remove(b"AA");
    let mut keep = id.is_some_and(|id| selected.contains(&id));
    if let Ok(kids) = dict.get(b"Kids") {
        let kids = kids
            .as_array()
            .map_err(|_| Error::Invalid("DOCUMENT_FORM".into()))?
            .clone();
        let mut retained = Vec::new();
        for kid in kids {
            if let Some(kid) = selected_form_field(doc, &kid, selected, depth + 1)? {
                retained.push(kid);
            }
        }
        keep |= !retained.is_empty();
        dict.set("Kids", retained);
    }
    if !keep {
        return Ok(None);
    }
    if let Some(id) = id {
        doc.objects.insert(id, dict.into());
        Ok(Some(field.clone()))
    } else {
        Ok(Some(dict.into()))
    }
}

fn disable_active_content(object: &mut lopdf::Object) {
    use lopdf::Object;
    let dict = match object {
        Object::Dictionary(dict) => dict,
        Object::Stream(stream) => &mut stream.dict,
        Object::Array(items) => {
            for item in items {
                disable_active_content(item);
            }
            return;
        }
        _ => return,
    };
    // Active payload objects are discarded too, not only their entry points.
    if dict
        .get(b"Type")
        .and_then(Object::as_name)
        .is_ok_and(|n| matches!(n, b"Action" | b"EmbeddedFile"))
        || dict.get(b"S").and_then(Object::as_name).is_ok_and(|n| {
            matches!(
                n,
                b"JavaScript"
                    | b"URI"
                    | b"Launch"
                    | b"GoTo"
                    | b"GoToR"
                    | b"GoToE"
                    | b"SubmitForm"
                    | b"ResetForm"
                    | b"ImportData"
                    | b"Rendition"
                    | b"Movie"
                    | b"Sound"
                    | b"Named"
                    | b"SetOCGState"
                    | b"Hide"
                    | b"Trans"
                    | b"GoTo3DView"
            )
        })
    {
        *object = Object::Null;
        return;
    }
    // Resource and appearance-state dictionaries use arbitrary names (including
    // A/AA/FS). Removing those names indiscriminately would erase visible glyphs,
    // fonts or selected appearance states. Only semantic PDF dictionaries have
    // action/file entry points; action payloads themselves were removed above.
    let semantic = dict
        .get(b"Type")
        .and_then(Object::as_name)
        .is_ok_and(|n| matches!(n, b"Catalog" | b"Page" | b"Pages" | b"Annot" | b"Filespec"))
        || dict
            .get(b"Subtype")
            .and_then(Object::as_name)
            .is_ok_and(|n| {
                matches!(
                    n,
                    b"Text"
                        | b"Link"
                        | b"FreeText"
                        | b"Line"
                        | b"Square"
                        | b"Circle"
                        | b"Polygon"
                        | b"PolyLine"
                        | b"Highlight"
                        | b"Underline"
                        | b"Squiggly"
                        | b"StrikeOut"
                        | b"Stamp"
                        | b"Caret"
                        | b"Ink"
                        | b"Popup"
                        | b"FileAttachment"
                        | b"Sound"
                        | b"Movie"
                        | b"Widget"
                        | b"Screen"
                        | b"PrinterMark"
                        | b"TrapNet"
                        | b"Watermark"
                        | b"3D"
                        | b"Redact"
                        | b"RichMedia"
                )
            })
        || dict
            .get(b"FT")
            .and_then(Object::as_name)
            .is_ok_and(|n| matches!(n, b"Tx" | b"Btn" | b"Ch" | b"Sig"))
        || dict.get(b"T").and_then(Object::as_str).is_ok()
        || dict.get(b"Fields").and_then(Object::as_array).is_ok();
    if semantic {
        for key in [
            b"A".as_slice(),
            b"AA",
            b"OpenAction",
            b"JS",
            b"JavaScript",
            b"URI",
            b"Launch",
            b"Next",
            b"EF",
            b"AF",
            b"FS",
            b"EmbeddedFiles",
            b"RichMediaContent",
            b"RichMediaSettings",
            b"Movie",
            b"Sound",
            b"XFA",
        ] {
            dict.remove(key);
        }
    }
    for (_, value) in dict.iter_mut() {
        disable_active_content(value);
    }
}

/// Parses and subsets with lopdf; None only inspects page count.
pub fn select_pdf(
    bytes: &[u8],
    range: Option<&PageRange>,
    max_pages: u32,
    max_bytes: usize,
) -> Result<(u32, Option<Vec<u8>>)> {
    if bytes.len() > max_bytes {
        return Err(Error::Invalid("DOCUMENT_BYTES_LIMIT".into()));
    }
    let mut doc =
        lopdf::Document::load_mem(bytes).map_err(|_| Error::Invalid("DOCUMENT_PDF".into()))?;
    if doc.is_encrypted() {
        return Err(Error::Unsupported("encrypted_pdf".into()));
    }
    let pages = doc.get_pages();
    let count =
        u32::try_from(pages.len()).map_err(|_| Error::Invalid("DOCUMENT_PAGE_COUNT".into()))?;
    if count == 0 {
        return Err(Error::Invalid("DOCUMENT_PAGE_COUNT".into()));
    }
    let Some(range) = range else {
        return Ok((count, None));
    };
    range.validate()?;
    if range.last > count || range.last - range.first + 1 > max_pages {
        return Err(Error::Invalid("DOCUMENT_PAGE_RANGE".into()));
    }
    let remove: Vec<_> = pages
        .keys()
        .copied()
        .filter(|p| *p < range.first || *p > range.last)
        .collect();
    doc.delete_pages(&remove);
    let selected = doc
        .get_pages()
        .values()
        .filter_map(|id| doc.get_dictionary(*id).ok())
        .filter_map(|page| page.get(b"Annots").ok())
        .filter_map(|annots| match annots {
            lopdf::Object::Reference(id) => doc.get_object(*id).ok(),
            value => Some(value),
        })
        .filter_map(|annots| annots.as_array().ok())
        .flatten()
        .filter_map(|annot| annot.as_reference().ok())
        .collect();
    let form = doc
        .catalog()
        .ok()
        .and_then(|c| c.get(b"AcroForm").ok())
        .cloned();
    let form = if let Some(form) = form {
        let form = match &form {
            lopdf::Object::Reference(id) => doc.get_object(*id),
            object => Ok(object),
        }
        .map_err(|_| Error::Invalid("DOCUMENT_FORM".into()))?;
        let mut form = form
            .as_dict()
            .map_err(|_| Error::Invalid("DOCUMENT_FORM".into()))?
            .clone();
        if form.has(b"XFA") {
            return Err(Error::Unsupported("document_xfa".into()));
        }
        // Calculation order can retain fields from excluded pages. Only the
        // selected field tree and the defaults needed to display it survive.
        let discard = form
            .iter()
            .map(|(k, _)| k.clone())
            .filter(|k| {
                ![b"Fields".as_slice(), b"DR", b"DA", b"NeedAppearances", b"Q"]
                    .contains(&k.as_slice())
            })
            .collect::<Vec<_>>();
        for key in discard {
            form.remove(&key);
        }
        let fields = form
            .get(b"Fields")
            .and_then(lopdf::Object::as_array)
            .map_err(|_| Error::Invalid("DOCUMENT_FORM".into()))?
            .clone();
        let mut retained = Vec::new();
        for field in fields {
            if let Some(field) = selected_form_field(&mut doc, &field, &selected, 0)? {
                retained.push(field);
            }
        }
        form.set("Fields", retained);
        Some(form)
    } else {
        None
    };
    doc.trailer.remove(b"Info");
    if let Ok(catalog) = doc.catalog_mut() {
        let keys: Vec<_> = catalog
            .iter()
            .map(|(key, _)| key.clone())
            .filter(|k| k != b"Type" && k != b"Pages" && k != b"OCProperties")
            .collect();
        for key in keys {
            catalog.remove(&key);
        }
        if let Some(form) = form {
            catalog.set("AcroForm", form);
        }
    }
    for id in doc.get_pages().values() {
        if let Ok(page) = doc.get_dictionary_mut(*id) {
            page.remove(b"AA");
            page.remove(b"Metadata");
        }
    }
    for object in doc.objects.values_mut() {
        disable_active_content(object);
    }
    let mut trailer = lopdf::Object::Dictionary(std::mem::take(&mut doc.trailer));
    disable_active_content(&mut trailer);
    doc.trailer = trailer
        .as_dict()
        .map_err(|_| Error::Invalid("DOCUMENT_PDF".into()))?
        .clone();
    doc.prune_objects();
    let mut output = Vec::new();
    doc.save_to(&mut output)
        .map_err(|_| Error::Invalid("DOCUMENT_PDF".into()))?;
    if output.len() > max_bytes {
        return Err(Error::Invalid("DOCUMENT_BYTES_LIMIT".into()));
    }
    Ok((count, Some(output)))
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentViewInput {
    pub attachment: Attachment,
    pub pages: Option<PageRange>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DocumentViewLimits {
    pub max_bytes: usize,
    pub max_selected_pages: u32,
    pub timeout: u64,
}
impl Default for DocumentViewLimits {
    fn default() -> Self {
        Self {
            max_bytes: 16 * 1024 * 1024,
            max_selected_pages: 32,
            timeout: 60,
        }
    }
}
pub struct DocumentView<S, P, R, C> {
    pub resolver: ImageResolver<S, P>,
    pub external: Option<Arc<dyn AttachmentResolver>>,
    pub runs: Arc<R>,
    pub clock: C,
    pub output_access: Access,
    pub limits: DocumentViewLimits,
}
#[derive(Serialize, Deserialize)]
struct Receipt {
    binding: String,
    output: Value,
}
impl<S: StandardToolStore, P: Policy, R: RunStore, C: Clock> DocumentView<S, P, R, C> {
    fn input(&self, input: Value) -> Result<DocumentViewInput> {
        let input: DocumentViewInput = serde_json::from_value(input)
            .map_err(|_| Error::Invalid("DOCUMENT_VIEW_INPUT".into()))?;
        let target = input.attachment.declaration();
        if target.bytes > self.limits.max_bytes as u64
            || self.limits.max_bytes == 0
            || self.limits.timeout == 0
            || self.limits.max_selected_pages == 0
        {
            return Err(Error::Invalid("DOCUMENT_BYTES_LIMIT".into()));
        }
        if !matches!(
            target.mime_type.as_str(),
            "application/pdf" | "image/png" | "image/jpeg" | "image/webp"
        ) {
            return Err(Error::Unsupported("document_view_mime".into()));
        }
        input.attachment.validate()?;
        if matches!(input.attachment, Attachment::External(_)) && self.external.is_none() {
            return Err(Error::Unsupported("document_view_external_resolver".into()));
        }
        if let Some(pages) = &input.pages {
            pages.validate()?;
            if pages.last - pages.first + 1 > self.limits.max_selected_pages {
                return Err(Error::Invalid("DOCUMENT_PAGE_RANGE".into()));
            }
        }
        Ok(input)
    }
    fn authorize(
        &self,
        run: &RunRecord,
        input: &DocumentViewInput,
        permission: &str,
    ) -> Result<()> {
        self.resolver.policy.authorize(
            run,
            &format!("attachment:{}", digest(&json!(input.attachment))),
            permission,
        )?;
        if let Attachment::Stored(reference) = &input.attachment {
            self.resolver.authorize(run, reference, permission)?;
        }
        Ok(())
    }
    async fn bound(&self, c: &ToolContext, input: &DocumentViewInput) -> Result<RunRecord> {
        if c.claim.run != self.output_access.actor().owner
            || self.output_access.actor() != self.output_access.namespace()
            || c.permission != "document.view"
            || c.account_binding != "documents"
        {
            return Err(Error::PermissionDenied);
        }
        let run = self.runs.get(&c.claim.run).await?;
        self.authorize(&run, input, "document.view")?;
        if let Attachment::Stored(reference) = &input.attachment {
            self.resolver
                .authorize_current(&run, reference, "document.view")
                .await?;
        }
        if let Attachment::External(target) = &input.attachment {
            self.external
                .as_ref()
                .ok_or(Error::PermissionDenied)?
                .authorize(c, "document.view", target)
                .map_err(|_| Error::PermissionDenied)?;
        }
        Ok(run)
    }
    fn binding(&self, c: &ToolContext, input: &Value) -> String {
        digest(&json!([
            c.claim.run,
            c.operation,
            c.attempt,
            input,
            self.limits,
            self.output_access.namespace()
        ]))
    }
    async fn stored(&self, c: &ToolContext, input: &Value) -> Result<Lookup> {
        let parsed = self.input(input.clone())?;
        let run = self.bound(c, &parsed).await?;
        let binding = self.binding(c, input);
        let staged = match self
            .resolver
            .store
            .staged(&self.output_access, &format!("document-result-{binding}"))
            .await
        {
            Ok(s) => s,
            Err(Error::NotFound) => return Ok(Lookup::Unknown),
            Err(e) => return Err(e),
        };
        if staged.expires_at <= self.clock.now() {
            return Ok(Lookup::Unknown);
        }
        let receipt: Receipt = serde_json::from_slice(&staged.bytes).map_err(|_| Error::Storage)?;
        if receipt.binding != binding {
            return Err(Error::Conflict);
        }
        self.authorize_result(&run, &receipt.output).await?;
        Ok(Lookup::Found(applied(receipt.output)))
    }
}
fn applied(output: Value) -> ExternalResult {
    ExternalResult::Applied {
        physical_evidence: None,
        output,
        usage: Usage::measured(0, 0, 0),
        external_id: None,
    }
}
impl<S: StandardToolStore, P: Policy, R: RunStore, C: Clock> Tool for DocumentView<S, P, R, C> {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: "document.view".into(),
            version: format!(
                "document-view-2:{}",
                digest(&json!([
                    self.limits,
                    self.output_access.namespace(),
                    self.resolver.limits
                ]))
            ),
            account: "documents".into(),
            permission: "document.view".into(),
            capability:
                "Inspect PDF page count or attach selected PDF pages / PNG / JPEG / WebP to the next model input"
                    .into(),
            input_schema: json!({"type":"object","properties":{"attachment":{"type":"object"},"pages":{"type":["object","null"],"properties":{"first":{"type":"integer","minimum":1},"last":{"type":"integer","minimum":1}},"required":["first","last"],"additionalProperties":false}},"required":["attachment"],"additionalProperties":false}),
            output_schema: json!({"type":"object"}),
            side_effect: true,
            timeout: self.limits.timeout,
            max_output_bytes: 16384,
            reserve: Amount::default(),
            quantities: Default::default(),
            conflict_scope: None,
            retry: RetryPolicy {
                max_attempts: 1,
                backoff: 0,
                max_elapsed: self.limits.timeout,
            },
        }
    }
    async fn quote(&self, run: &RunRecord, input: &Value) -> Result<Amount> {
        let input = self.input(input.clone())?;
        self.authorize(run, &input, "document.view")?;
        if let Attachment::Stored(reference) = &input.attachment {
            self.resolver
                .authorize_current(run, reference, "document.view")
                .await?;
        }
        Ok(Amount::default())
    }
    async fn authorize_result(&self, run: &RunRecord, output: &Value) -> Result<()> {
        let input = self.input(output["source"].clone())?;
        self.authorize(run, &input, "attachment.retrieve")?;
        if let Attachment::Stored(reference) = &input.attachment {
            self.resolver
                .authorize_current(run, reference, "attachment.retrieve")
                .await?;
        }
        for key in ["attachments", "image_inputs"] {
            if let Some(refs) = output.get(key).and_then(Value::as_array) {
                for reference in refs {
                    let reference: ImageReference = if key == "attachments" {
                        match serde_json::from_value::<Attachment>(reference.clone())
                            .map_err(|_| Error::Storage)?
                        {
                            Attachment::Stored(r) => r,
                            _ => return Err(Error::Storage),
                        }
                    } else {
                        serde_json::from_value(reference.clone()).map_err(|_| Error::Storage)?
                    };
                    self.resolver
                        .authorize_current(run, &reference, "attachment.retrieve")
                        .await?;
                }
            }
        }
        Ok(())
    }
    async fn lookup(&self, c: &ToolContext, input: Value) -> Result<Lookup> {
        self.stored(c, &input).await
    }
    async fn invoke(&self, c: &ToolContext, input: Value) -> Result<ExternalResult> {
        let parsed = self.input(input.clone())?;
        let run = self.bound(c, &parsed).await?;
        if let Lookup::Found(result) = self.stored(c, &input).await? {
            return Ok(result);
        }
        let target = parsed.attachment.declaration();
        let bytes = match &parsed.attachment {
            Attachment::Stored(reference) => {
                self.resolver
                    .read_reference(&run, reference, "attachment.resolve", self.limits.max_bytes)
                    .await?
            }
            Attachment::External(_) => {
                let result = self
                    .external
                    .as_ref()
                    .ok_or(Error::PermissionDenied)?
                    .resolve(c, "document.view", &target)
                    .await
                    .map_err(|_| Error::Invalid("ATTACHMENT_RESOLUTION".into()))?;
                result.validate(&target)?;
                match result.data() {
                    AttachmentData::Bytes(bytes) => bytes.clone(),
                    AttachmentData::Url(_) => {
                        return Err(Error::Unsupported("document_view_requires_bytes".into()));
                    }
                }
            }
        };
        let (count, selected) = if target.mime_type == "application/pdf" {
            select_pdf(
                &bytes,
                parsed.pages.as_ref(),
                self.limits.max_selected_pages,
                self.limits.max_bytes,
            )?
        } else {
            self.resolver.limits.inspect(&target.mime_type, &bytes)?;
            if parsed
                .pages
                .as_ref()
                .is_some_and(|r| r.first != 1 || r.last != 1)
            {
                return Err(Error::Invalid("DOCUMENT_PAGE_RANGE".into()));
            }
            (1, Some(bytes))
        };
        let mut output = json!({"page_count":count,"source":parsed});
        let binding = self.binding(c, &input);
        if let Some(selected) = selected {
            let current = self.bound(c, &parsed).await?;
            self.resolver
                .policy
                .authorize(&current, "document.view", "attachment.save")?;
            let staged = self
                .resolver
                .store
                .stage_named(
                    &self.output_access,
                    ArtifactSpec {
                        id: format!("document-{binding}"),
                        media_type: target.mime_type.clone(),
                        class: DataClass::Intermediate,
                    },
                    &selected,
                    self.clock.now(),
                )
                .await?;
            let reference = ImageReference::Artifact(
                self.resolver
                    .store
                    .save(&self.output_access, staged, self.clock.now())
                    .await?,
            );
            if target.mime_type.starts_with("image/") {
                output["image_inputs"] = json!([reference]);
            } else {
                output["attachments"] = json!([Attachment::Stored(reference)]);
            }
        }
        let receipt = serde_json::to_vec(&Receipt {
            binding: binding.clone(),
            output: output.clone(),
        })
        .map_err(|_| Error::Storage)?;
        self.resolver
            .store
            .stage_named(
                &self.output_access,
                ArtifactSpec {
                    id: format!("document-result-{binding}"),
                    media_type: "application/json".into(),
                    class: DataClass::Checkpoint,
                },
                &receipt,
                self.clock.now(),
            )
            .await?;
        Ok(applied(output))
    }
}

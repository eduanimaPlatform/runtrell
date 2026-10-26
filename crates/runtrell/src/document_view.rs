// SPDX-License-Identifier: Apache-2.0
//! Optional page inspection/selection. Original data is never returned as tool JSON.
use crate::{attachments::*, data::*, images::*, *};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

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
    // Discard document-wide metadata/actions and page annotations. Only the
    // selected page tree and its rendering resources belong to this view.
    doc.trailer.remove(b"Info");
    if let Ok(catalog) = doc.catalog_mut() {
        let keys: Vec<_> = catalog
            .iter()
            .map(|(key, _)| key.clone())
            .filter(|k| k != b"Type" && k != b"Pages")
            .collect();
        for key in keys {
            catalog.remove(&key);
        }
    }
    for id in doc.get_pages().values() {
        if let Ok(page) = doc.get_dictionary_mut(*id) {
            page.remove(b"Annots");
            page.remove(b"AA");
            page.remove(b"Metadata");
        }
    }
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
        if !matches!(target.mime_type.as_str(), "application/pdf" | "image/png") {
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
                "document-view-1:{}",
                digest(&json!([
                    self.limits,
                    self.output_access.namespace(),
                    self.resolver.limits
                ]))
            ),
            account: "documents".into(),
            permission: "document.view".into(),
            capability:
                "Inspect page count or attach selected PDF pages / PNG to the next model input"
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
            (1, parsed.pages.as_ref().map(|_| bytes))
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
            if target.mime_type == "image/png" {
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

// SPDX-License-Identifier: Apache-2.0
use crate::{
    adapter::{Adapter, Wire},
    *,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use runtrell::{attachments::*, *};
use serde_json::json;
use std::time::{Duration, SystemTime};

const MAX_URL_BYTES: usize = 8192;
fn invalid() -> Error {
    Error::Invalid("ATTACHMENT".into())
}
impl Adapter {
    pub(crate) fn check_input_attachments(
        &self,
        attachments: &[Attachment],
        images: &[runtrell::images::ImageReference],
    ) -> Result<()> {
        let limit = || Error::Invalid("MODEL_ATTACHMENT_LIMIT".into());
        let total = attachments.iter().try_fold(0u64, |n, a| {
            n.checked_add(a.declaration().bytes).ok_or_else(limit)
        })?;
        if attachments.len() > self.config.document_limits.max_inputs
            || total > self.config.document_limits.max_total_bytes
            || images.len() > self.config.image_limits.max_images
            || images
                .iter()
                .any(|i| i.bytes() > self.config.image_limits.max_image_bytes as u64)
        {
            return Err(limit());
        }
        for attachment in attachments {
            self.document_transports(&attachment.declaration())?;
        }
        if !images.is_empty() && !self.config.capabilities.images.input {
            return Err(Error::Unsupported("image_input".into()));
        }
        if images.iter().any(|i| {
            !self
                .config
                .image_limits
                .allowed_mime
                .iter()
                .any(|mime| mime == i.mime())
        }) {
            return Err(Error::Unsupported("image_input_mime".into()));
        }
        Ok(())
    }
    fn document_transports(&self, input: &ExternalAttachment) -> Result<&DocumentTransports> {
        input.validate()?;
        if self.wire == Wire::Chat {
            return Err(Error::Unsupported("document_input".into()));
        }
        let c = &self.config.capabilities.documents;
        let support = if input.mime_type == "application/pdf" {
            &c.pdf
        } else {
            &c.image
        };
        if !support.url && !support.inline_bytes {
            return Err(Error::Unsupported("document_input".into()));
        }
        Ok(support)
    }
    /// Conservative additional JSON bytes. No resolution or request mutation.
    pub(crate) fn document_bound(&self, request: &PreparedModelRequest) -> Result<usize> {
        let inputs = request.attachments();
        if inputs.is_empty() {
            return Ok(0);
        }
        if (self.config.attachment_resolver.is_none()
            && inputs.iter().any(|a| matches!(a, Attachment::External(_))))
            || inputs.len() > self.config.document_limits.max_inputs
        {
            return Err(invalid());
        }
        let mut total = 0u64;
        let mut wire = 0usize;
        for input in inputs {
            let input = input.declaration();
            let support = self.document_transports(&input)?;
            total = total.checked_add(input.bytes).ok_or_else(invalid)?;
            if total > self.config.document_limits.max_total_bytes {
                return Err(invalid());
            }
            let inline = if support.inline_bytes {
                usize::try_from(input.bytes)
                    .map_err(|_| invalid())?
                    .div_ceil(3)
                    .checked_mul(4)
                    .ok_or_else(invalid)?
            } else {
                0
            };
            let url = if support.url { MAX_URL_BYTES * 6 } else { 0 };
            wire = wire
                .checked_add(inline.max(url))
                .and_then(|n| n.checked_add(512))
                .ok_or_else(invalid)?;
        }
        Ok(wire)
    }
    pub(crate) fn check_document_wire_bound(
        &self,
        request: &PreparedModelRequest,
        body: &Value,
    ) -> Result<()> {
        if serde_json::to_vec(body)
            .map_err(|_| invalid())?
            .len()
            .checked_add(self.document_bound(request)?)
            .is_none_or(|n| n > self.config.max_request_bytes)
        {
            return Err(invalid());
        }
        Ok(())
    }
    pub(crate) async fn resolve_documents(
        &self,
        context: &ToolContext,
        request: &PreparedModelRequest,
    ) -> Result<Vec<ResolvedAttachment>> {
        self.document_bound(request)?;
        if request.attachments().is_empty() {
            return Ok(vec![]);
        }
        self.authorize_attachments(context, request)?;
        let mut timeout = Duration::from_secs(self.config.capabilities.limits.timeout);
        if let Some(deadline) = context.deadline {
            let now = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_err(|_| Error::Deadline)?
                .as_secs();
            if deadline <= now {
                return Err(Error::Deadline);
            }
            timeout = timeout.min(Duration::from_secs(deadline - now));
        }
        tokio::time::timeout(timeout, async {
            let mut resolved = Vec::new();
            let mut stored = request.prepared_attachments().iter();
            for attachment in request.attachments() {
                let input = attachment.declaration();
                let result = match attachment {
                    Attachment::External(_) => self
                        .config
                        .attachment_resolver
                        .as_ref()
                        .ok_or_else(invalid)?
                        .resolve(context, &self.config.endpoint, &input)
                        .await
                        .map_err(|_| Error::Invalid("ATTACHMENT_RESOLUTION".into()))?,
                    Attachment::Stored(_) => stored.next().ok_or_else(invalid)?.clone(),
                };
                result.validate(&input)?;
                let support = self.document_transports(&input)?;
                match result.data() {
                    AttachmentData::Bytes(_) if !support.inline_bytes => {
                        return Err(Error::Unsupported("document_inline".into()));
                    }
                    AttachmentData::Url(_) if !support.url => {
                        return Err(Error::Unsupported("document_url".into()));
                    }
                    AttachmentData::Url(url) => {
                        let parsed = reqwest::Url::parse(url).map_err(|_| invalid())?;
                        if url.len() > MAX_URL_BYTES
                            || parsed.scheme() != "https"
                            || parsed.host_str().is_none()
                            || !parsed.username().is_empty()
                            || parsed.password().is_some()
                            || parsed.fragment().is_some()
                        {
                            return Err(invalid());
                        }
                    }
                    _ => (),
                }
                resolved.push(result);
            }
            Ok(resolved)
        })
        .await
        .map_err(|_| Error::Deadline)?
    }
    pub(crate) fn authorize_attachments(
        &self,
        context: &ToolContext,
        request: &PreparedModelRequest,
    ) -> Result<()> {
        for input in request.attachments() {
            if let Attachment::External(input) = input {
                self.config
                    .attachment_resolver
                    .as_ref()
                    .ok_or_else(invalid)?
                    .authorize(context, &self.config.endpoint, input)
                    .map_err(|_| Error::PermissionDenied)?;
            }
        }
        Ok(())
    }
    pub(crate) fn attach_documents(
        &self,
        body: &mut Value,
        request: &PreparedModelRequest,
        resolved: &[ResolvedAttachment],
    ) {
        if resolved.is_empty() {
            return;
        }
        let parts: Vec<_> = request.attachments().iter().zip(resolved).map(|(input, result)| {
            let input = input.declaration();
            match (self.wire, result.data(), input.mime_type.as_str()) {
                (Wire::Responses, AttachmentData::Url(url), "application/pdf") => json!({"type":"input_file","file_url":url}),
                (Wire::Responses, AttachmentData::Bytes(bytes), "application/pdf") => json!({"type":"input_file","filename":"document.pdf","file_data":format!("data:application/pdf;base64,{}",STANDARD.encode(bytes))}),
                (Wire::Responses, data, _) => {
                    let url = match data { AttachmentData::Url(url) => url.clone(), AttachmentData::Bytes(bytes) => format!("data:{};base64,{}",input.mime_type,STANDARD.encode(bytes)) };
                    json!({"type":"input_image","image_url":url})
                }
                (Wire::GenerateContent, AttachmentData::Url(url), _) => json!({"fileData":{"mimeType":input.mime_type,"fileUri":url}}),
                (Wire::GenerateContent, AttachmentData::Bytes(bytes), _) => json!({"inlineData":{"mimeType":input.mime_type,"data":STANDARD.encode(bytes)}}),
                (Wire::Chat, _, _) => unreachable!("document capability validated before resolution"),
            }
        }).collect();
        match self.wire {
            Wire::Responses => body["input"]
                .as_array_mut()
                .unwrap()
                .push(json!({"role":"user","content":parts})),
            Wire::GenerateContent => body["contents"]
                .as_array_mut()
                .unwrap()
                .push(json!({"role":"user","parts":parts})),
            Wire::Chat => (),
        }
    }
}

/// Do not persist locators/payloads echoed by a provider, including opaque state.
pub(crate) fn redact_echoes(value: &mut Value, resolved: &[ResolvedAttachment]) {
    if resolved.is_empty() {
        return;
    }
    let mut secrets = Vec::new();
    for input in resolved {
        match input.data() {
            AttachmentData::Url(url) => secrets.push(url.clone()),
            AttachmentData::Bytes(bytes) => {
                secrets.push(STANDARD.encode(bytes));
                if let Ok(text) = std::str::from_utf8(bytes) {
                    secrets.push(text.into());
                }
            }
        }
    }
    fn redact(value: &mut Value, secrets: &[String]) {
        let clean = |s: &str| {
            secrets
                .iter()
                .filter(|s| !s.is_empty())
                .fold(s.to_owned(), |text, secret| {
                    text.replace(secret, "[REDACTED]")
                })
        };
        match value {
            Value::String(s) => *s = clean(s),
            Value::Array(items) => {
                for item in items {
                    redact(item, secrets);
                }
            }
            Value::Object(items) => {
                *items = std::mem::take(items)
                    .into_iter()
                    .map(|(key, mut value)| {
                        redact(&mut value, secrets);
                        (clean(&key), value)
                    })
                    .collect();
            }
            _ => (),
        }
    }
    redact(value, &secrets);
}

// SPDX-License-Identifier: Apache-2.0
//! Fixed attachment identity, with optional ephemeral host resolution.
use crate::{Error, Result, ToolContext, data::content_digest, images::ImageReference};
use serde::{Deserialize, Serialize};
use std::{future::Future, pin::Pin};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageRange {
    pub first: u32,
    pub last: u32,
}
impl PageRange {
    pub fn validate(&self) -> Result<()> {
        if self.first == 0 || self.last < self.first {
            return Err(Error::Invalid("ATTACHMENT_PAGE_RANGE".into()));
        }
        Ok(())
    }
}
/// Opaque IDs/version, never locators or credentials. Fixed before estimation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalAttachment {
    pub reference_id: String,
    pub version: String,
    pub pages: Option<PageRange>,
    pub mime_type: String,
    pub bytes: u64,
    pub digest: String,
}
fn opaque(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 256
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
}
impl ExternalAttachment {
    pub fn validate(&self) -> Result<()> {
        if !opaque(&self.reference_id)
            || !opaque(&self.version)
            || self.bytes == 0
            || self.digest.len() != 64
            || !self
                .digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || !matches!(
                self.mime_type.as_str(),
                "application/pdf" | "image/png" | "image/jpeg" | "image/webp"
            )
        {
            return Err(Error::Invalid("ATTACHMENT_DECLARATION".into()));
        }
        if let Some(pages) = &self.pages {
            pages.validate()?;
        }
        Ok(())
    }
}
/// Reuses the existing fixed-version Workspace/Artifact contract used by images.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Attachment {
    External(ExternalAttachment),
    Stored(ImageReference),
}
impl Attachment {
    pub fn declaration(&self) -> ExternalAttachment {
        match self {
            Self::External(input) => input.clone(),
            Self::Stored(reference) => ExternalAttachment {
                reference_id: crate::digest(&serde_json::json!(reference)),
                version: match reference {
                    ImageReference::Artifact(a) => a.revision.to_string(),
                    ImageReference::Workspace { file, .. } => file.revision.to_string(),
                },
                pages: None,
                mime_type: reference.mime().into(),
                bytes: reference.bytes(),
                digest: reference.hash().into(),
            },
        }
    }
    pub fn validate(&self) -> Result<()> {
        self.declaration().validate()
    }
}
pub(crate) fn model_attachments(messages: &[crate::Message]) -> Result<Vec<Attachment>> {
    let mut attachments = Vec::new();
    for message in active_tool_messages(messages) {
        if let Some(value) = message.data.get("attachments") {
            let refs: Vec<Attachment> = serde_json::from_value(value.clone())
                .map_err(|_| Error::Invalid("ATTACHMENT_REFERENCE".into()))?;
            for reference in refs {
                if !attachments.contains(&reference) {
                    attachments.push(reference);
                }
            }
        }
    }
    Ok(attachments)
}
/// A tool exchange ends at a final assistant response. Partial max_tokens
/// responses retain the exchange, including across checkpoint recovery.
pub(crate) fn active_tool_messages(
    messages: &[crate::Message],
) -> impl Iterator<Item = &crate::Message> {
    let start = messages
        .iter()
        .rposition(|m| {
            m.role == "assistant" && (m.call_id.is_some() || m.data["continuation"] != true)
        })
        .map_or(0, |i| i + 1);
    messages[start..].iter().filter(|m| m.role == "tool")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Message;
    #[test]
    fn only_latest_exchange_is_attached_but_history_is_unchanged() {
        let tool = |id: &str| {
            let mut m = Message::text("tool", "");
            m.data = serde_json::json!({"attachments":[Attachment::External(ExternalAttachment {
                reference_id:id.into(), version:"v1".into(), pages:Some(PageRange {first:1,last:2}),
                mime_type:"application/pdf".into(), bytes:3, digest:content_digest(b"abc")
            })]});
            m
        };
        let mut messages = vec![
            tool("old"),
            Message::text("assistant", "next"),
            tool("new"),
            tool("other"),
            tool("new"),
        ];
        let before = messages.clone();
        let selected = model_attachments(&messages).unwrap();
        assert_eq!(
            selected
                .iter()
                .map(|a| a.declaration().reference_id)
                .collect::<Vec<_>>(),
            ["new", "other"]
        );
        assert_eq!(messages, before);
        messages.push(Message::text("assistant", "save"));
        messages.push(Message::text("tool", "saved markdown"));
        assert!(model_attachments(&messages).unwrap().is_empty());
        messages.push(tool("old"));
        assert_eq!(
            model_attachments(&messages).unwrap()[0]
                .declaration()
                .reference_id,
            "old"
        );
    }
}
pub struct ResolutionError;
impl std::fmt::Debug for ResolutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ResolutionError")
    }
}
impl std::fmt::Display for ResolutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("attachment resolution failed")
    }
}
impl std::error::Error for ResolutionError {}
pub type ResolveFuture<'a> = Pin<
    Box<dyn Future<Output = std::result::Result<ResolvedAttachment, ResolutionError>> + Send + 'a>,
>;
pub trait AttachmentResolver: Send + Sync {
    /// Current authorization only: no URL issuance, content acquisition or inference.
    fn authorize(
        &self,
        context: &ToolContext,
        destination: &str,
        input: &ExternalAttachment,
    ) -> std::result::Result<(), ResolutionError>;
    /// Authorize the exact fixed target and destination again; never log locators/bytes.
    fn resolve<'a>(
        &'a self,
        context: &'a ToolContext,
        destination: &'a str,
        input: &'a ExternalAttachment,
    ) -> ResolveFuture<'a>;
}
#[derive(Clone)]
pub enum AttachmentData {
    Url(String),
    Bytes(Vec<u8>),
}
impl std::fmt::Debug for AttachmentData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AttachmentData([REDACTED])")
    }
}
#[derive(Clone)]
pub struct ResolvedAttachment {
    data: AttachmentData,
    target: ExternalAttachment,
}
impl std::fmt::Debug for ResolvedAttachment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ResolvedAttachment([REDACTED])")
    }
}
impl ResolvedAttachment {
    /// Measures and hashes actual bytes; the caller supplies fixed version/range/MIME.
    pub fn bytes(mut target: ExternalAttachment, bytes: Vec<u8>) -> Self {
        target.digest = content_digest(&bytes);
        target.bytes = bytes.len() as u64;
        Self {
            target,
            data: AttachmentData::Bytes(bytes),
        }
    }
    /// Resolver attests that this URL serves exactly this fixed target and metadata.
    /// Runtrell does not independently fetch the URL target.
    pub fn url(target: ExternalAttachment, url: impl Into<String>) -> Self {
        Self {
            data: AttachmentData::Url(url.into()),
            target,
        }
    }
    pub fn validate(&self, input: &ExternalAttachment) -> Result<()> {
        input.validate()?;
        if self.target != *input {
            return Err(Error::Conflict);
        }
        Ok(())
    }
    pub fn data(&self) -> &AttachmentData {
        &self.data
    }
}

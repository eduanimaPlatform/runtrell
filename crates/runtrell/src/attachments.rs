// SPDX-License-Identifier: Apache-2.0
//! Fixed attachment identity, with optional ephemeral host resolution.
use crate::{
    Error, Message, ModelRequest, Result, ToolContext, data::content_digest, images::ImageReference,
};
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
    pub fn bytes(&self) -> u64 {
        match self {
            Self::External(input) => input.bytes,
            Self::Stored(reference) => reference.bytes(),
        }
    }

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
#[derive(Default)]
pub(crate) struct ToolMedia {
    pub attachments: Vec<Attachment>,
    pub images: Vec<ImageReference>,
}
impl ToolMedia {
    fn from_message(message: &Message) -> Result<Self> {
        let mut media = Self {
            images: image_references(message)?,
            ..Self::default()
        };
        if let Some(value) = message.data.get("attachments") {
            media.attachments = Vec::deserialize(value)
                .map_err(|_| Error::Invalid("ATTACHMENT_REFERENCE".into()))?;
        }
        Ok(media)
    }

    pub(crate) fn collect(messages: &[Message], initial_images: &[ImageReference]) -> Result<Self> {
        let mut media = Self {
            images: initial_images.to_vec(),
            ..Self::default()
        };
        for message in messages.iter().filter(|m| m.role == "tool") {
            let next = Self::from_message(message)?;
            extend_unique(&mut media.images, next.images);
            extend_unique(&mut media.attachments, next.attachments);
        }
        Ok(media)
    }
}

fn image_references(message: &Message) -> Result<Vec<ImageReference>> {
    message
        .data
        .get("image_inputs")
        .map(Vec::deserialize)
        .transpose()
        .map(|images| images.unwrap_or_default())
        .map_err(|_| Error::Invalid("IMAGE_REFERENCE".into()))
}

fn extend_unique<T: PartialEq>(items: &mut Vec<T>, next: Vec<T>) {
    for item in next {
        if !items.contains(&item) {
            items.push(item);
        }
    }
}

/// Index of a declared input in a ModelRequest's flat media lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaInput {
    Image(usize),
    Attachment(usize),
}

impl ModelRequest {
    /// Media after each message, preceded by a slot for media before any messages.
    /// Tool results from one parallel exchange stay together before their media.
    /// Only references included in the request's declared lists can be placed.
    pub fn media_slots(&self) -> Result<Vec<Vec<MediaInput>>> {
        let mut slots = vec![Vec::new(); self.messages.len() + 1];
        if self.images.is_empty() && self.attachments.is_empty() {
            return Ok(slots);
        }

        let first_user = self.messages.iter().position(|m| m.role == "user");
        let initial_slot = first_user.map_or(0, |i| i + 1);
        let mut images = vec![false; self.images.len()];
        let mut attachments = vec![false; self.attachments.len()];
        let initial_images = first_user
            .filter(|_| !self.images.is_empty())
            .map(|i| image_references(&self.messages[i]))
            .transpose()?
            .unwrap_or_default();
        for (image, placed) in self.images.iter().zip(&mut images) {
            *placed = initial_images.contains(image);
        }
        let mut batch = Vec::new();
        for (i, message) in self.messages.iter().enumerate() {
            if message.role != "tool" {
                continue;
            }
            let media = ToolMedia::from_message(message)?;
            place(
                &media.images,
                &self.images,
                &mut images,
                &mut batch,
                MediaInput::Image,
            );
            place(
                &media.attachments,
                &self.attachments,
                &mut attachments,
                &mut batch,
                MediaInput::Attachment,
            );
            if self
                .messages
                .get(i + 1)
                .is_none_or(|next| next.role != "tool")
            {
                slots[i + 1] = std::mem::take(&mut batch);
            }
        }
        // Re-add the pinned images and place explicit host inputs with the first
        // user message. An image declared in that message stays there on reopen.
        slots[initial_slot].extend(self.images.iter().enumerate().filter_map(|(i, image)| {
            (!images[i] || initial_images.contains(image)).then_some(MediaInput::Image(i))
        }));
        slots[initial_slot].extend(
            attachments
                .iter()
                .enumerate()
                .filter_map(|(i, seen)| (!seen).then_some(MediaInput::Attachment(i))),
        );
        Ok(slots)
    }
}

fn place<T: PartialEq>(
    references: &[T],
    declared: &[T],
    placed: &mut [bool],
    batch: &mut Vec<MediaInput>,
    input: fn(usize) -> MediaInput,
) {
    for reference in references {
        if let Some(i) = declared.iter().position(|item| item == reference)
            && !std::mem::replace(&mut placed[i], true)
        {
            batch.push(input(i));
        }
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

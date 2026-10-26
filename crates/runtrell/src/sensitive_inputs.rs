// SPDX-License-Identifier: Apache-2.0
//! Host-only, immutable document declarations and ephemeral transport resolution.
use crate::{Error, Result, ToolContext, data::content_digest};
use serde::{Deserialize, Serialize};
use std::{future::Future, pin::Pin};

/// Only this declaration is saved. IDs must be opaque, never URLs or credentials.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SensitiveInput {
    pub reference_id: String,
    pub mime_type: String,
    pub bytes: u64,
    /// Lowercase SHA-256 of the fixed original, known before reservation.
    pub digest: String,
}
impl SensitiveInput {
    pub fn validate(&self) -> Result<()> {
        if self.reference_id.is_empty()
            || self.reference_id.len() > 256
            || !self
                .reference_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
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
            return Err(Error::Invalid("SENSITIVE_INPUT_DECLARATION".into()));
        }
        Ok(())
    }
}

/// No serialization or payload-bearing Debug/Display, including failures.
pub struct ResolutionError;
impl std::fmt::Debug for ResolutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ResolutionError")
    }
}
impl std::fmt::Display for ResolutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("sensitive input resolution failed")
    }
}
impl std::error::Error for ResolutionError {}
pub type ResolveFuture<'a> = Pin<
    Box<
        dyn Future<Output = std::result::Result<ResolvedSensitiveInput, ResolutionError>>
            + Send
            + 'a,
    >,
>;

/// Authorize the trusted context, fixed target/version and destination each time.
/// Implementations must not log locators or bytes. No result is cached by Runtrell.
pub trait SensitiveInputResolver: Send + Sync {
    fn resolve<'a>(
        &'a self,
        context: &'a ToolContext,
        destination: &'a str,
        input: &'a SensitiveInput,
    ) -> ResolveFuture<'a>;
}

pub enum SensitiveInputData {
    Url(String),
    Bytes(Vec<u8>),
}
impl std::fmt::Debug for SensitiveInputData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SensitiveInputData([REDACTED])")
    }
}
pub struct ResolvedSensitiveInput {
    data: SensitiveInputData,
    bytes: u64,
    digest: String,
}
impl std::fmt::Debug for ResolvedSensitiveInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ResolvedSensitiveInput([REDACTED])")
    }
}
impl ResolvedSensitiveInput {
    pub fn bytes(bytes: Vec<u8>) -> Self {
        Self {
            digest: content_digest(&bytes),
            bytes: bytes.len() as u64,
            data: SensitiveInputData::Bytes(bytes),
        }
    }
    /// Host attests the content of the immutable URL target, not the URL string.
    pub fn url(url: impl Into<String>, bytes: u64, digest: impl Into<String>) -> Self {
        Self {
            data: SensitiveInputData::Url(url.into()),
            bytes,
            digest: digest.into(),
        }
    }
    pub fn validate(&self, input: &SensitiveInput) -> Result<()> {
        input.validate()?;
        if self.bytes != input.bytes || self.digest != input.digest {
            return Err(Error::Conflict);
        }
        Ok(())
    }
    pub fn data(&self) -> &SensitiveInputData {
        &self.data
    }
}

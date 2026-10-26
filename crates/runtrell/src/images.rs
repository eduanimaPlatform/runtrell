// SPDX-License-Identifier: Apache-2.0
//! Optional image contracts, authorized resolution and ordinary single-operation tools.
use crate::{data::*, *};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{ops::Deref, sync::Arc};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImageReference {
    Artifact(ArtifactRef),
    Workspace {
        namespace: Namespace,
        file: FileVersion,
        mime_type: String,
    },
}
pub(crate) fn model_images(run: &RunRecord, messages: &[Message]) -> Result<Vec<ImageReference>> {
    let mut images = run.spec.images.clone();
    for message in messages.iter().filter(|m| m.role == "tool") {
        if let Some(value) = message.data.get("image_inputs") {
            let refs: Vec<ImageReference> =
                serde_json::from_value(value.clone()).map_err(|_| invalid("IMAGE_REFERENCE"))?;
            for reference in refs {
                if !images.contains(&reference) {
                    images.push(reference);
                }
            }
        }
    }
    Ok(images)
}
impl ImageReference {
    pub fn namespace(&self) -> &Namespace {
        match self {
            Self::Artifact(a) => &a.owner,
            Self::Workspace { namespace, .. } => namespace,
        }
    }
    pub fn hash(&self) -> &str {
        match self {
            Self::Artifact(a) => &a.hash,
            Self::Workspace { file, .. } => &file.hash,
        }
    }
    pub fn bytes(&self) -> u64 {
        match self {
            Self::Artifact(a) => a.bytes,
            Self::Workspace { file, .. } => file.bytes,
        }
    }
    pub fn mime(&self) -> &str {
        match self {
            Self::Artifact(a) => &a.media_type,
            Self::Workspace { mime_type, .. } => mime_type,
        }
    }
    pub fn policy_target(&self) -> String {
        format!("image:{}", digest(&json!(self)))
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageLimits {
    pub max_image_bytes: usize,
    pub max_request_bytes: usize,
    pub max_response_bytes: usize,
    pub max_images: usize,
    pub max_width: u32,
    pub max_height: u32,
    pub max_pixels: u64,
    pub allowed_mime: Vec<String>,
}
impl Default for ImageLimits {
    fn default() -> Self {
        Self {
            max_image_bytes: 8 * 1024 * 1024,
            max_request_bytes: 48 * 1024 * 1024,
            max_response_bytes: 48 * 1024 * 1024,
            max_images: 4,
            max_width: 8192,
            max_height: 8192,
            max_pixels: 16_777_216,
            allowed_mime: vec!["image/png".into()],
        }
    }
}
fn invalid(code: &str) -> Error {
    Error::Invalid(code.into())
}
impl ImageLimits {
    pub fn decode(&self, mime: &str, data_base64: &str) -> Result<Vec<u8>> {
        if data_base64.len() > self.max_image_bytes.div_ceil(3).saturating_mul(4) {
            return Err(invalid("IMAGE_BYTES_LIMIT"));
        }
        let bytes = STANDARD
            .decode(data_base64)
            .map_err(|_| invalid("IMAGE_BASE64"))?;
        self.inspect(mime, &bytes)?;
        Ok(bytes)
    }
    pub fn validate(&self) -> Result<()> {
        let wire = self
            .max_image_bytes
            .div_ceil(3)
            .checked_mul(4)
            .and_then(|n| n.checked_mul(self.max_images))
            .and_then(|n| n.checked_add(65536))
            .ok_or_else(|| invalid("IMAGE_LIMIT_CONFIGURATION"))?;
        if self.max_image_bytes == 0
            || self.max_images == 0
            || self.max_width == 0
            || self.max_height == 0
            || self.max_pixels == 0
            || self.allowed_mime.is_empty()
            || self.allowed_mime.iter().any(|m| m != "image/png")
            || self.max_request_bytes < wire
            || self.max_response_bytes < wire
        {
            return Err(invalid("IMAGE_LIMIT_CONFIGURATION"));
        }
        Ok(())
    }
    /// Validate PNG chunk structure, CRCs and dimensions without decoding pixels.
    /// Pixel/semantic validation belongs to the host's validator.
    pub fn inspect(&self, mime: &str, bytes: &[u8]) -> Result<(u32, u32)> {
        if bytes.len() > self.max_image_bytes {
            return Err(invalid("IMAGE_BYTES_LIMIT"));
        }
        if !self.allowed_mime.iter().any(|m| m == mime) || mime != "image/png" {
            return Err(invalid("IMAGE_MIME_UNSUPPORTED"));
        }
        if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err(invalid("IMAGE_FORMAT"));
        }
        let mut offset = 8usize;
        let mut dimensions = None;
        let mut data = false;
        let mut ended_data = false;
        let mut palette = false;
        while offset.checked_add(12).is_some_and(|n| n <= bytes.len()) {
            let size = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
            let end = offset
                .checked_add(12)
                .and_then(|n| n.checked_add(size))
                .filter(|n| *n <= bytes.len())
                .ok_or_else(|| invalid("IMAGE_FORMAT"))?;
            let kind = &bytes[offset + 4..offset + 8];
            let payload = &bytes[offset + 8..end - 4];
            let expected = u32::from_be_bytes(bytes[end - 4..end].try_into().unwrap());
            let mut crc = !0u32;
            for b in &bytes[offset + 4..end - 4] {
                crc ^= u32::from(*b);
                for _ in 0..8 {
                    crc = (crc >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(crc & 1));
                }
            }
            if !crc != expected || !kind.iter().all(u8::is_ascii_alphabetic) {
                return Err(invalid("IMAGE_FORMAT"));
            }
            match kind {
                b"IHDR" if offset == 8 && size == 13 => {
                    let width = u32::from_be_bytes(payload[..4].try_into().unwrap());
                    let height = u32::from_be_bytes(payload[4..8].try_into().unwrap());
                    if width == 0
                        || height == 0
                        || width > self.max_width
                        || height > self.max_height
                        || u64::from(width) * u64::from(height) > self.max_pixels
                    {
                        return Err(invalid("IMAGE_DIMENSIONS_LIMIT"));
                    }
                    let depth_ok = match payload[9] {
                        0 => matches!(payload[8], 1 | 2 | 4 | 8 | 16),
                        2 | 4 | 6 => matches!(payload[8], 8 | 16),
                        3 => matches!(payload[8], 1 | 2 | 4 | 8),
                        _ => false,
                    };
                    if !depth_ok || payload[10] != 0 || payload[11] != 0 || payload[12] > 1 {
                        return Err(invalid("IMAGE_FORMAT"));
                    }
                    dimensions = Some((width, height));
                }
                b"PLTE"
                    if dimensions.is_some()
                        && !data
                        && !palette
                        && size > 0
                        && size <= 768
                        && size.is_multiple_of(3) =>
                {
                    palette = true
                }
                b"IDAT" if dimensions.is_some() && !ended_data && size > 0 => data = true,
                b"IEND" if size == 0 && data && end == bytes.len() => {
                    return dimensions.ok_or_else(|| invalid("IMAGE_FORMAT"));
                }
                _ if dimensions.is_some() && kind[0].is_ascii_lowercase() => {
                    ended_data |= data;
                }
                _ => return Err(invalid("IMAGE_FORMAT")),
            }
            offset = end;
        }
        Err(invalid("IMAGE_FORMAT"))
    }
}
/// Ephemeral transport payload, never serialized into checkpoints or operation input.
#[derive(Clone)]
pub struct ResolvedImage {
    reference: ImageReference,
    mime_type: String,
    data_base64: String,
}
impl std::fmt::Debug for ResolvedImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedImage")
            .field("reference", &self.reference)
            .finish_non_exhaustive()
    }
}
impl ResolvedImage {
    pub fn validate(&self, limits: &ImageLimits) -> Result<()> {
        limits.decode(&self.mime_type, &self.data_base64)?;
        Ok(())
    }
    pub fn reference(&self) -> &ImageReference {
        &self.reference
    }
    pub fn mime_type(&self) -> &str {
        &self.mime_type
    }
    pub fn data_base64(&self) -> &str {
        &self.data_base64
    }
}
/// No mutable dereference or attachment setter: sizing and sending share this request.
#[derive(Clone, Debug)]
pub struct PreparedModelRequest {
    request: ModelRequest,
    images: Vec<ResolvedImage>,
    attachments: Vec<crate::attachments::ResolvedAttachment>,
}
impl Deref for PreparedModelRequest {
    type Target = ModelRequest;
    fn deref(&self) -> &ModelRequest {
        &self.request
    }
}
impl PreparedModelRequest {
    /// Lookup reconstruction deliberately has no payload. Never silently omit
    /// declared local inputs when such a request is passed to invoke or sizing.
    pub fn validate_for_send(&self) -> Result<()> {
        if self.request.images.len() != self.images.len()
            || self
                .request
                .images
                .iter()
                .zip(&self.images)
                .any(|(reference, image)| reference != image.reference())
            || self
                .request
                .attachments
                .iter()
                .filter(|a| matches!(a, crate::attachments::Attachment::Stored(_)))
                .count()
                != self.attachments.len()
        {
            return Err(Error::Invalid("MODEL_INPUT_NOT_PREPARED".into()));
        }
        Ok(())
    }
    /// Reconstruct identity for result lookup only, without acquiring original data.
    /// ModelPort::lookup must authorize the references before returning any result.
    pub fn for_lookup(request: ModelRequest) -> Result<Self> {
        for input in &request.attachments {
            input.validate()?;
        }
        Ok(Self {
            request,
            images: vec![],
            attachments: vec![],
        })
    }
    pub fn text(request: ModelRequest) -> Result<Self> {
        if !request.images.is_empty() {
            return Err(Error::Unsupported("image_input".into()));
        }
        for input in &request.attachments {
            input.validate()?;
            if matches!(input, crate::attachments::Attachment::Stored(_)) {
                return Err(Error::Unsupported(
                    "stored_attachment_requires_preparation".into(),
                ));
            }
        }
        Ok(Self {
            request,
            images: vec![],
            attachments: vec![],
        })
    }
    pub fn request(&self) -> &ModelRequest {
        &self.request
    }
    pub fn attachments(&self) -> &[crate::attachments::Attachment] {
        &self.request.attachments
    }
    pub fn prepared_attachments(&self) -> &[crate::attachments::ResolvedAttachment] {
        &self.attachments
    }
    pub fn resolved_images(&self) -> &[ResolvedImage] {
        &self.images
    }
    pub fn with_max_output(mut self, cap: u64) -> Self {
        self.request.max_output = cap;
        self
    }
}
pub struct ImageResolver<S, P> {
    pub store: S,
    pub policy: P,
    pub accesses: Vec<Access>,
    pub limits: ImageLimits,
}
impl<S: StandardToolStore, P: Policy> ImageResolver<S, P> {
    /// Validate current grants via metadata without acquiring file content.
    pub async fn authorize_current(
        &self,
        run: &RunRecord,
        reference: &ImageReference,
        permission: &str,
    ) -> Result<()> {
        let access = self.authorize(run, reference, permission)?;
        self.store.snapshot(access).await?;
        Ok(())
    }
    pub fn authorize(
        &self,
        run: &RunRecord,
        reference: &ImageReference,
        permission: &str,
    ) -> Result<&Access> {
        self.policy
            .authorize(run, &reference.policy_target(), permission)?;
        self.accesses
            .iter()
            .find(|a| a.actor().owner == run.handle && a.namespace() == reference.namespace())
            .ok_or(Error::PermissionDenied)
    }
    pub async fn resolve(
        &self,
        run: &RunRecord,
        references: &[ImageReference],
    ) -> Result<Vec<ResolvedImage>> {
        self.limits.validate()?;
        if references.len() > self.limits.max_images {
            return Err(invalid("IMAGE_COUNT_LIMIT"));
        }
        let mut images = vec![];
        for reference in references {
            let bytes = self
                .read_reference(run, reference, "image.resolve", self.limits.max_image_bytes)
                .await?;
            self.limits.inspect(reference.mime(), &bytes)?;
            images.push(ResolvedImage {
                reference: reference.clone(),
                mime_type: reference.mime().into(),
                data_base64: STANDARD.encode(bytes),
            });
        }
        Ok(images)
    }
    pub async fn read_reference(
        &self,
        run: &RunRecord,
        reference: &ImageReference,
        permission: &str,
        max_bytes: usize,
    ) -> Result<Vec<u8>> {
        let access = self.authorize(run, reference, permission)?;
        let length =
            usize::try_from(reference.bytes()).map_err(|_| invalid("IMAGE_BYTES_LIMIT"))?;
        if length == 0 || length > max_bytes {
            return Err(invalid("ATTACHMENT_BYTES_LIMIT"));
        }
        let mut bytes = Vec::with_capacity(length);
        while bytes.len() < length {
            let chunk = (length - bytes.len()).min(self.store.policy().max_read);
            let part = match reference {
                ImageReference::Artifact(a) => {
                    self.store
                        .read_artifact(access, a, bytes.len() as u64, chunk)
                        .await?
                }
                ImageReference::Workspace { file, .. } => {
                    self.store
                        .read(access, &file.path, file.revision, bytes.len() as u64, chunk)
                        .await?
                }
            };
            if part.total != reference.bytes()
                || part.offset != bytes.len() as u64
                || part.bytes.is_empty()
                || part.bytes.len() > chunk
            {
                return Err(Error::Conflict);
            }
            bytes.extend_from_slice(&part.bytes);
        }
        if content_digest(&bytes) != reference.hash() {
            return Err(Error::Conflict);
        }
        Ok(bytes)
    }
}
/// Attaches owned storage and Policy to any image-capable model adapter.
pub struct ImageModel<M, S, P, R> {
    pub inner: M,
    pub resolver: ImageResolver<S, P>,
    pub runs: Arc<R>,
}
impl<M: ModelPort, S: StandardToolStore, P: Policy, R: RunStore> ModelPort
    for ImageModel<M, S, P, R>
{
    fn compaction_limits(&self) -> ModelLimits {
        self.inner.compaction_limits()
    }
    fn limits(&self) -> ModelLimits {
        self.inner.limits()
    }
    fn input_upper_bound(&self, r: &PreparedModelRequest) -> Result<u64> {
        self.inner.input_upper_bound(r)
    }
    fn retry_policy(&self) -> RetryPolicy {
        self.inner.retry_policy()
    }
    fn recovery(&self) -> Option<(String, u64)> {
        self.inner.recovery()
    }
    fn requires_recovery_preparation(&self) -> bool {
        self.inner.requires_recovery_preparation()
    }
    async fn prepare_recovery(
        &self,
        c: &PreparationContext,
        v: &Value,
    ) -> Result<Option<RecoveryProof>> {
        self.inner.prepare_recovery(c, v).await
    }
    async fn cancel(&self, c: &ToolContext) -> Result<Progress> {
        self.inner.cancel(c).await
    }
    async fn prepare_request(
        &self,
        run: &RunRecord,
        request: ModelRequest,
    ) -> Result<PreparedModelRequest> {
        for input in &request.attachments {
            input.validate()?;
        }
        let images = self.resolver.resolve(run, &request.images).await?;
        let mut attachments = Vec::new();
        for input in &request.attachments {
            if let crate::attachments::Attachment::Stored(reference) = input {
                let bytes = self
                    .resolver
                    .read_reference(
                        run,
                        reference,
                        "attachment.resolve",
                        self.resolver.limits.max_request_bytes,
                    )
                    .await?;
                attachments.push(crate::attachments::ResolvedAttachment::bytes(
                    input.declaration(),
                    bytes,
                ));
            }
        }
        Ok(PreparedModelRequest {
            request,
            images,
            attachments,
        })
    }
    async fn invoke(
        &self,
        c: &ToolContext,
        request: PreparedModelRequest,
    ) -> Result<ExternalResult> {
        request.validate_for_send()?;
        let run = self.runs.get(&c.claim.run).await?;
        for image in request.resolved_images() {
            self.resolver
                .authorize_current(&run, image.reference(), "image.send")
                .await?;
        }
        for input in request.attachments() {
            if let crate::attachments::Attachment::Stored(reference) = input {
                self.resolver
                    .authorize_current(&run, reference, "attachment.send")
                    .await?;
            }
        }
        self.inner.invoke(c, request).await
    }
    async fn lookup(&self, c: &ToolContext, request: PreparedModelRequest) -> Result<Lookup> {
        let run = self.runs.get(&c.claim.run).await?;
        for image in request.resolved_images() {
            self.resolver
                .authorize_current(&run, image.reference(), "image.retrieve")
                .await?;
        }
        for reference in &request.request().images {
            self.resolver
                .authorize_current(&run, reference, "image.retrieve")
                .await?;
        }
        for input in request.attachments() {
            if let crate::attachments::Attachment::Stored(reference) = input {
                self.resolver
                    .authorize_current(&run, reference, "attachment.retrieve")
                    .await?;
            }
        }
        self.inner.lookup(c, request).await
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImageAction {
    Generate,
    Edit,
    MaskEdit,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageRequest {
    pub prompt: String,
    #[serde(default)]
    pub references: Vec<ImageReference>,
    pub mask: Option<ImageReference>,
}
#[derive(Clone, Debug)]
pub struct PreparedImageRequest {
    request: ImageRequest,
    images: Vec<ResolvedImage>,
    mask: Option<ResolvedImage>,
    action: ImageAction,
}
impl PreparedImageRequest {
    pub fn prompt(&self) -> &str {
        &self.request.prompt
    }
    pub fn images(&self) -> &[ResolvedImage] {
        &self.images
    }
    pub fn mask(&self) -> Option<&ResolvedImage> {
        self.mask.as_ref()
    }
    pub fn action(&self) -> ImageAction {
        self.action
    }
}
#[derive(Clone, Debug)]
pub struct EncodedImage {
    pub mime_type: String,
    pub data_base64: String,
}
#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)] // Inline receipt forwarded directly into settlement.
pub enum ImageResponse {
    Received {
        images: Vec<EncodedImage>,
        usage: Usage,
        failure: Option<String>,
    },
    Unknown,
}
pub trait ImagePort: Send + Sync {
    /// Model/endpoint/limits identity; credentials must never be included.
    fn identity(&self) -> String;
    fn check(&self, request: &PreparedImageRequest) -> Result<()>;
    fn quote(&self, request: &PreparedImageRequest) -> Result<Amount> {
        self.check(request)?;
        Ok(Amount::default())
    }
    fn invoke(
        &self,
        context: &ToolContext,
        request: PreparedImageRequest,
    ) -> impl std::future::Future<Output = Result<ImageResponse>> + Send;
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImageToolConfig {
    pub action: ImageAction,
    pub reserve: Amount,
    pub timeout: u64,
}
pub struct ImageTool<I, S, P, R, C> {
    pub provider: I,
    pub resolver: ImageResolver<S, P>,
    pub runs: Arc<R>,
    pub clock: C,
    pub output_access: Access,
    pub config: ImageToolConfig,
}
#[derive(Serialize, Deserialize)]
struct Receipt {
    binding: String,
    result: ExternalResult,
}
impl<I: ImagePort, S: StandardToolStore, P: Policy, R: RunStore, C: Clock>
    ImageTool<I, S, P, R, C>
{
    fn name(&self) -> &str {
        match self.config.action {
            ImageAction::Generate => "image.generate",
            _ => "image.edit",
        }
    }
    fn binding(&self, c: &ToolContext, input: &Value) -> String {
        digest(&json!([
            c.claim.run,
            c.operation,
            c.attempt,
            input,
            self.provider.identity(),
            self.config,
            self.resolver.limits,
            self.output_access.namespace()
        ]))
    }
    async fn bound(&self, c: &ToolContext) -> Result<RunRecord> {
        if c.claim.run != self.output_access.actor().owner
            || self.output_access.actor() != self.output_access.namespace()
            || c.permission != self.name()
            || c.account_binding != "images"
        {
            return Err(Error::PermissionDenied);
        }
        let run = self.runs.get(&c.claim.run).await?;
        self.resolver
            .policy
            .authorize(&run, self.name(), c.permission.as_str())?;
        Ok(run)
    }
    async fn prepare(&self, run: &RunRecord, input: &Value) -> Result<PreparedImageRequest> {
        let request: ImageRequest =
            serde_json::from_value(input.clone()).map_err(|_| invalid("IMAGE_REQUEST"))?;
        if request.prompt.is_empty() || request.prompt.len() > 65536 || self.config.timeout == 0 {
            return Err(invalid("IMAGE_REQUEST"));
        }
        match self.config.action {
            ImageAction::Generate if !request.references.is_empty() || request.mask.is_some() => {
                return Err(invalid("IMAGE_REQUEST"));
            }
            ImageAction::Edit if request.references.is_empty() || request.mask.is_some() => {
                return Err(invalid("IMAGE_REQUEST"));
            }
            ImageAction::MaskEdit if request.references.is_empty() || request.mask.is_none() => {
                return Err(invalid("IMAGE_REQUEST"));
            }
            _ => (),
        }
        if request.references.len() + usize::from(request.mask.is_some())
            > self.resolver.limits.max_images
        {
            return Err(invalid("IMAGE_COUNT_LIMIT"));
        }
        let images = self.resolver.resolve(run, &request.references).await?;
        let mask = self
            .resolver
            .resolve(run, &request.mask.clone().into_iter().collect::<Vec<_>>())
            .await?
            .pop();
        let prepared = PreparedImageRequest {
            request,
            images,
            mask,
            action: self.config.action,
        };
        self.provider.check(&prepared)?;
        Ok(prepared)
    }
    async fn stored(&self, c: &ToolContext, input: &Value) -> Result<Lookup> {
        let run = self.bound(c).await?;
        let binding = self.binding(c, input);
        self.resolver.policy.authorize(
            &run,
            &format!("image-result:{binding}"),
            "image.retrieve",
        )?;
        let staged = match self
            .resolver
            .store
            .staged(&self.output_access, &format!("image-result-{binding}"))
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
        if let ExternalResult::Applied { output, .. } = &receipt.result {
            let refs: Vec<ImageReference> = serde_json::from_value(output["image_inputs"].clone())
                .map_err(|_| Error::Storage)?;
            for r in &refs {
                self.resolver.authorize(&run, r, "image.retrieve")?;
            }
            self.resolver.resolve(&run, &refs).await?;
        }
        Ok(Lookup::Found(receipt.result))
    }
}
impl<I: ImagePort, S: StandardToolStore, P: Policy, R: RunStore, C: Clock> Tool
    for ImageTool<I, S, P, R, C>
{
    async fn authorize_result(&self, run: &RunRecord, output: &Value) -> Result<()> {
        let refs: Vec<ImageReference> =
            serde_json::from_value(output["image_inputs"].clone()).map_err(|_| Error::Storage)?;
        for reference in &refs {
            self.resolver.authorize(run, reference, "image.retrieve")?;
        }
        self.resolver.resolve(run, &refs).await?;
        Ok(())
    }
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: self.name().into(),
            version: format!(
                "images-1:{}",
                digest(&json!([
                    self.provider.identity(),
                    self.config,
                    self.resolver.limits,
                    self.output_access.namespace()
                ]))
            ),
            account: "images".into(),
            permission: self.name().into(),
            capability: "image-generation-editing".into(),
            input_schema: json!({"type":"object","properties":{"prompt":{"type":"string","minLength":1},"references":{"type":"array","items":{"type":"object"}},"mask":{"type":["object","null"]}},"required":["prompt"],"additionalProperties":false}),
            output_schema: json!({"type":"object"}),
            side_effect: true,
            timeout: self.config.timeout,
            max_output_bytes: 65536,
            reserve: self.config.reserve,
            quantities: Default::default(),
            conflict_scope: None,
            retry: RetryPolicy {
                max_attempts: 1,
                backoff: 0,
                max_elapsed: self.config.timeout,
            },
        }
    }
    async fn quote(&self, run: &RunRecord, input: &Value) -> Result<Amount> {
        let request = self.prepare(run, input).await?;
        let quote = self.provider.quote(&request)?;
        Ok(Amount::new(
            quote.tokens.max(self.config.reserve.tokens),
            quote.micro_usd.max(self.config.reserve.micro_usd),
        ))
    }
    async fn lookup(&self, c: &ToolContext, input: Value) -> Result<Lookup> {
        self.stored(c, &input).await
    }
    async fn invoke(&self, c: &ToolContext, input: Value) -> Result<ExternalResult> {
        let run = self.bound(c).await?;
        if let Lookup::Found(result) = self.stored(c, &input).await? {
            return Ok(result);
        }
        let request = self.prepare(&run, &input).await?;
        for image in request.images().iter().chain(request.mask()) {
            self.resolver
                .authorize_current(&run, image.reference(), "image.send")
                .await?;
        }
        self.resolver
            .policy
            .authorize(&run, self.name(), "image.save")?;
        let response = self.provider.invoke(c, request).await?;
        let ImageResponse::Received {
            images,
            usage,
            failure,
        } = response
        else {
            return Ok(ExternalResult::Unknown);
        };
        // Unknown usage is never normalized to free. Preserve known results too;
        // Runtime retains the reservation until the usage can be settled.
        let binding = self.binding(c, &input);
        let mut failure = failure;
        let mut references = vec![];
        if images.is_empty() || images.len() > self.resolver.limits.max_images {
            failure.get_or_insert("IMAGE_COUNT_LIMIT".into());
        }
        if failure.is_none() {
            for (i, image) in images.into_iter().enumerate() {
                let bytes = match self
                    .resolver
                    .limits
                    .decode(&image.mime_type, &image.data_base64)
                {
                    Ok(b) => b,
                    Err(e) => {
                        failure = Some(e.to_string());
                        break;
                    }
                };
                let saved = async {
                    let current = self.runs.get(&c.claim.run).await?;
                    self.resolver
                        .policy
                        .authorize(&current, self.name(), "image.save")?;
                    let staged = self
                        .resolver
                        .store
                        .stage_named(
                            &self.output_access,
                            ArtifactSpec {
                                id: format!("image-{binding}-{i}"),
                                media_type: image.mime_type,
                                class: DataClass::Intermediate,
                            },
                            &bytes,
                            self.clock.now(),
                        )
                        .await?;
                    self.resolver
                        .store
                        .save(&self.output_access, staged, self.clock.now())
                        .await
                }
                .await;
                match saved {
                    Ok(reference) => references.push(ImageReference::Artifact(reference)),
                    Err(_) => {
                        failure = Some("IMAGE_SAVE_FAILED".into());
                        break;
                    }
                }
            }
        }
        let result = if let Some(code) = failure {
            ExternalResult::Failed {
                physical_evidence: None,
                code,
                usage,
                retry_after: None,
            }
        } else {
            ExternalResult::Applied {
                physical_evidence: None,
                output: json!({"image_inputs":references}),
                usage,
                external_id: None,
            }
        };
        let receipt = serde_json::to_vec(&Receipt {
            binding: binding.clone(),
            result: result.clone(),
        })
        .map_err(|_| Error::Storage)?;
        let stored = self
            .resolver
            .store
            .stage_named(
                &self.output_access,
                ArtifactSpec {
                    id: format!("image-result-{binding}"),
                    media_type: "application/json".into(),
                    class: DataClass::Checkpoint,
                },
                &receipt,
                self.clock.now(),
            )
            .await;
        if stored.is_err() {
            let usage = match result {
                ExternalResult::Applied { usage, .. } | ExternalResult::Failed { usage, .. } => {
                    usage
                }
                _ => return Ok(ExternalResult::Unknown),
            };
            return Ok(ExternalResult::Failed {
                physical_evidence: None,
                code: "IMAGE_RECEIPT_SAVE_FAILED".into(),
                usage,
                retry_after: None,
            });
        }
        Ok(result)
    }
}

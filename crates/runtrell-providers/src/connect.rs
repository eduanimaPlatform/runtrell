// SPDX-License-Identifier: Apache-2.0
use crate::{
    adapter::{Adapter, Wire},
    *,
};
use runtrell::models::{OperationKind, ResolveError, ResolvedModel, resolve_binding};
use runtrell::{Error, Result};

/// Return the credential for a resolved public model ID. The resolver runs
/// only after request validation and reservation of a physical send.
pub type CredentialResolver = Arc<dyn Fn(&str) -> Result<ApiKey> + Send + Sync>;

/// Business limits on a catalog binding.
#[derive(Clone)]
pub struct Connection {
    pub run: RunHandle,
    pub session: Option<String>,
    pub credentials: CredentialResolver,
    pub attachment_resolver: Option<Arc<dyn AttachmentResolver>>,
    pub retry_policy: RetryPolicy,
    pub timeout_ms: u64,
    /// Limit the sum of prompt tokens and the allowed generation for each request.
    pub context_cap: Option<u64>,
    /// Additional per-request limit on billed prompt tokens.
    pub input_token_cap: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub reasoning: Option<String>,
}

fn wire(model: &ResolvedModel) -> std::result::Result<Wire, ResolveError> {
    match model.api_family() {
        "openai_responses" | "openai_images" if cfg!(feature = "openai") => Ok(Wire::Responses),
        "gemini_generate" if cfg!(feature = "gemini") => Ok(Wire::GenerateContent),
        "openrouter_chat" if cfg!(feature = "openrouter") => Ok(Wire::Chat),
        _ => Err(ResolveError::ReservationUnavailable),
    }
}

/// Resolve a completion and connect its bundled adapter. Credentials are lazy.
pub fn connect_model(
    id: &str,
    binding: Option<&str>,
    connection: Connection,
) -> std::result::Result<ManagedModel, ConnectError> {
    let model = resolve_binding(id, OperationKind::Completion, binding)?;
    let wire = wire(&model)?;
    Ok(ManagedModel(adapter(model, wire, connection)?))
}

#[cfg(feature = "openai")]
impl OpenAiImages {
    /// Connect a single raster operation from the bundled catalog.
    pub fn connect(
        id: &str,
        kind: OperationKind,
        binding: Option<&str>,
        connection: Connection,
    ) -> std::result::Result<Self, ConnectError> {
        if kind == OperationKind::SvgGenerate {
            return Err(Error::Unsupported("direct SVG author max_output_tokens".into()).into());
        }
        if !matches!(
            kind,
            OperationKind::RasterGenerate | OperationKind::RasterEdit
        ) {
            return Err(ResolveError::UnsupportedOperation.into());
        }
        let model = resolve_binding(id, kind, binding)?;
        if model.api_family() != "openai_images" {
            return Err(ResolveError::UnsupportedOperation.into());
        }
        Ok(Self::from_catalog(adapter(
            model,
            Wire::Responses,
            connection,
        )?))
    }
}

#[cfg(feature = "gemini")]
impl GeminiImages {
    /// Connect a single raster operation from the bundled catalog.
    pub fn connect(
        id: &str,
        kind: OperationKind,
        binding: Option<&str>,
        connection: Connection,
    ) -> std::result::Result<Self, ConnectError> {
        if !matches!(
            kind,
            OperationKind::RasterGenerate | OperationKind::RasterEdit
        ) {
            return Err(ResolveError::UnsupportedOperation.into());
        }
        let model = resolve_binding(id, kind, binding)?;
        if model.api_family() != "gemini_generate" {
            return Err(ResolveError::UnsupportedOperation.into());
        }
        Ok(Self(adapter(model, Wire::GenerateContent, connection)?))
    }
}

/// Errors are safe to display before resolving a credential.
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error(transparent)]
    Resolve(#[from] ResolveError),
    #[error(transparent)]
    Configuration(#[from] Error),
}

fn lower(limit: u64, requested: Option<u64>) -> Result<u64> {
    let value = requested.unwrap_or(limit);
    if value == 0 || value > limit {
        return Err(Error::Invalid(
            "requested limit exceeds catalog bounds or is zero".into(),
        ));
    }
    Ok(value)
}

fn adapter(model: ResolvedModel, wire: Wire, connection: Connection) -> Result<Adapter> {
    if model.kind() != OperationKind::Completion
        && (connection.context_cap.is_some()
            || connection.input_token_cap.is_some()
            || connection.max_output_tokens.is_some())
    {
        return Err(Error::Unsupported(
            "completion token caps on an image binding".into(),
        ));
    }
    let caps = model.capabilities();
    let limits = model.limits();
    let provider_context = limits.context_tokens.ok_or(Error::ReservationDenied)?;
    let provider_input = limits.max_input_tokens.unwrap_or(provider_context);
    let context = lower(provider_context, connection.context_cap)?;
    let min_output = if model.api_family() == "openai_responses" {
        16
    } else {
        1
    };
    let absolute_output = limits.max_output_tokens.ok_or(Error::ReservationDenied)?;
    let output = lower(absolute_output, connection.max_output_tokens)?;
    let requested_input = lower(provider_input, connection.input_token_cap)?;
    let input = if context < provider_context {
        requested_input.min(context.saturating_sub(output))
    } else {
        requested_input
    };
    if input == 0
        || output < min_output
        || (input < provider_input && model.api_family() != "openai_responses")
    {
        return Err(Error::ReservationDenied);
    }
    if connection
        .reasoning
        .as_ref()
        .is_some_and(|mode| !caps.reasoning_modes.contains(mode))
    {
        return Err(Error::Unsupported("reasoning mode".into()));
    }
    let size = |quantity: Option<u64>| -> Result<usize> {
        usize::try_from(quantity.ok_or(Error::ReservationDenied)?)
            .map_err(|_| Error::ReservationDenied)
    };
    let max_request_bytes = size(limits.request_bytes)?;
    let max_response_bytes = size(limits.response_bytes)?;
    let document = model.inputs().iter().any(|input| input == "document");
    let image = model.inputs().iter().any(|input| input == "image");
    let image_count: usize = limits
        .images_in
        .unwrap_or(1)
        .try_into()
        .map_err(|_| Error::ReservationDenied)?;
    let max_image_bytes = max_request_bytes
        .min(max_response_bytes)
        .saturating_sub(65_536)
        / image_count.max(1)
        / 4
        * 3;
    let policy = if wire == Wire::GenerateContent {
        ImageSizePolicy::Unsized
    } else {
        ImageSizePolicy::Values {
            sizes: caps.image_sizes.clone(),
            auto: caps.auto_size,
        }
    };
    let generation = model.kind() == OperationKind::RasterGenerate;
    let reference_edit = model.kind() == OperationKind::RasterEdit;
    let svg_generation = model.kind() == OperationKind::SvgGenerate;
    let max_edge = model
        .image_reservation()
        .map_or(8192, |rule| rule.reference_long_edge);
    let config = Config {
        endpoint: model.endpoint(),
        #[cfg(test)]
        key: ApiKey::new(""),
        run: connection.run,
        session: connection.session,
        capabilities: Capabilities {
            limits: ModelLimits {
                model: model.wire_model().into(),
                context,
                max_output: output,
                min_max_output_tokens: min_output,
                price: None,
                timeout: connection.timeout_ms,
            },
            json_schema: caps.structured_output,
            tool_calling: caps.tools.iter().any(|name| name == "function"),
            parallel_tool_calls: false,
            images: ImageCapabilities {
                input: image,
                generation,
                reference_edit,
                svg_generation,
                generation_size: generation.then(|| policy.clone()),
                reference_edit_size: reference_edit.then(|| policy.clone()),
                svg_generation_size: svg_generation.then_some(policy),
                response_model_aliases: model.response_aliases().to_vec(),
                ..ImageCapabilities::default()
            },
            documents: DocumentCapabilities {
                pdf: DocumentTransports {
                    url: false,
                    inline_bytes: document,
                },
                image: DocumentTransports {
                    url: false,
                    inline_bytes: image,
                },
            },
            counters: CounterSupport {
                input: true,
                output: true,
                cached: true,
                reasoning: true,
                image_input: image,
                image_output: generation || reference_edit,
                cached_image_input: false,
            },
        },
        input_token_upper_bound: input,
        retry_policy: connection.retry_policy,
        max_request_bytes,
        max_response_bytes,
        image_limits: ImageLimits {
            max_request_bytes,
            max_response_bytes,
            max_image_bytes,
            max_images: image_count,
            max_width: max_edge,
            max_height: max_edge,
            max_pixels: u64::from(max_edge).pow(2).min(16_777_216),
            ..ImageLimits::default()
        },
        document_limits: DocumentLimits {
            max_inputs: limits
                .attachment_count
                .unwrap_or(0)
                .try_into()
                .map_err(|_| Error::ReservationDenied)?,
            max_total_bytes: limits.attachment_bytes.unwrap_or(0),
        },
        attachment_resolver: connection.attachment_resolver,
    };
    Adapter::managed(
        config,
        wire,
        model,
        connection.credentials,
        connection.reasoning,
    )
}

// SPDX-License-Identifier: Apache-2.0
use crate::{
    adapter::{Adapter, HttpResult, Wire},
    *,
};
use runtrell::{images::*, *};
use serde_json::json;

/// Optional native generateContent image generation and reference-image editing.
/// No remote lookup, mask support, hidden retry or idempotency claim.
#[derive(Debug)]
pub struct GeminiImages(Adapter);
impl GeminiImages {
    pub fn new(mut config: Config) -> Result<Self> {
        if config.capabilities.images.mask_edit {
            return Err(Error::Unsupported("mask_edit".into()));
        }
        config.max_request_bytes = config
            .max_request_bytes
            .min(config.image_limits.max_request_bytes);
        config.max_response_bytes = config
            .max_response_bytes
            .min(config.image_limits.max_response_bytes);
        if config.max_request_bytes < config.image_limits.max_request_bytes
            || config.max_response_bytes < config.image_limits.max_response_bytes
        {
            return Err(Error::Invalid("IMAGE_TRANSPORT_LIMIT_CONFIGURATION".into()));
        }
        Adapter::new(config, Wire::GenerateContent).map(Self)
    }
    pub fn capabilities(&self) -> &Capabilities {
        &self.0.config.capabilities
    }
    fn body(&self, request: &PreparedImageRequest) -> Result<Value> {
        let config = &self.0.config;
        let supported = match request.action() {
            ImageAction::Generate => config.capabilities.images.generation,
            ImageAction::Edit => config.capabilities.images.reference_edit,
            ImageAction::MaskEdit => false,
        };
        if !supported {
            return Err(Error::Unsupported(format!("{:?}", request.action())));
        }
        if request.mask().is_some() || request.images().len() > config.image_limits.max_images {
            return Err(Error::Invalid("IMAGE_COUNT_LIMIT".into()));
        }
        let mut parts = vec![json!({"text":request.prompt()})];
        for image in request.images() {
            image.validate(&config.image_limits)?;
            parts.push(
                json!({"inlineData":{"mimeType":image.mime_type(),"data":image.data_base64()}}),
            );
        }
        let body = json!({"contents":[{"role":"user","parts":parts}],"generationConfig":{"responseModalities":["TEXT","IMAGE"],"candidateCount":1,"maxOutputTokens":config.capabilities.limits.max_output}});
        if serde_json::to_vec(&body).map_err(|_| Error::Storage)?.len() > config.max_request_bytes {
            return Err(Error::Invalid("IMAGE_REQUEST_BYTES_LIMIT".into()));
        }
        Ok(body)
    }
}
impl ImagePort for GeminiImages {
    fn identity(&self) -> String {
        let c = &self.0.config;
        digest(&json!([
            "gemini-images-1",
            c.endpoint,
            c.capabilities,
            c.run,
            c.session,
            c.image_limits,
            c.input_token_upper_bound,
            c.max_request_bytes,
            c.max_response_bytes
        ]))
    }
    fn check(&self, request: &PreparedImageRequest) -> Result<()> {
        self.body(request).map(|_| ())
    }
    fn quote(&self, request: &PreparedImageRequest) -> Result<Amount> {
        self.check(request)?;
        self.0.config.capabilities.limits.estimate(
            self.0.config.input_token_upper_bound,
            self.0.config.capabilities.limits.max_output,
        )
    }
    async fn invoke(
        &self,
        context: &ToolContext,
        request: PreparedImageRequest,
    ) -> Result<ImageResponse> {
        let body = self.body(&request)?;
        let parsed = match self.0.send(context, body, &[]).await? {
            HttpResult::Json(parsed) => parsed,
            HttpResult::Failure(ExternalResult::Failed { code, usage, .. }) => {
                return Ok(ImageResponse::Received {
                    images: vec![],
                    usage,
                    failure: Some(code),
                });
            }
            _ => return Ok(ImageResponse::Unknown),
        };
        let mut usage = match self.0.usage(&parsed) {
            Ok(u) => u,
            Err(_) => return Ok(ImageResponse::Unknown),
        };
        let mut images = vec![];
        let mut failure = None;
        if let Some(candidates) = parsed["candidates"].as_array() {
            for candidate in candidates {
                if candidate["finishReason"] != "STOP" {
                    failure = Some("IMAGE_INCOMPLETE".into());
                }
                if let Some(parts) = candidate["content"]["parts"].as_array() {
                    for part in parts {
                        if part.get("inlineData").is_some() {
                            match (
                                part["inlineData"]["mimeType"].as_str(),
                                part["inlineData"]["data"].as_str(),
                            ) {
                                (Some(m), Some(d)) => images.push(EncodedImage {
                                    mime_type: m.into(),
                                    data_base64: d.into(),
                                }),
                                _ => failure = Some("IMAGE_FORMAT".into()),
                            }
                        }
                    }
                }
            }
        }
        usage
            .quantities
            .insert("image_input_count".into(), request.images().len() as u64);
        usage
            .quantities
            .insert("image_output_count".into(), images.len() as u64);
        if images.is_empty() || images.len() > self.0.config.image_limits.max_images {
            failure = Some("IMAGE_COUNT_LIMIT".into());
        }
        for image in &images {
            if let Err(error) = self
                .0
                .config
                .image_limits
                .decode(&image.mime_type, &image.data_base64)
            {
                failure = Some(error.to_string());
                break;
            }
        }
        Ok(ImageResponse::Received {
            images,
            usage,
            failure,
        })
    }
}

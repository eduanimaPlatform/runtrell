// SPDX-License-Identifier: Apache-2.0
use crate::{
    adapter::{Adapter, HttpResult, Wire},
    *,
};
use runtrell::{images::*, *};
use serde_json::json;

/// Optional Images API generation and reference editing. The host supplies the
/// full generations endpoint and model. No mask, remote lookup or hidden retry.
#[derive(Debug)]
pub struct OpenAiImages {
    adapter: Adapter,
    edits_endpoint: String,
}
impl OpenAiImages {
    pub fn new(mut config: Config) -> Result<Self> {
        if config.capabilities.images.mask_edit {
            return Err(Error::Unsupported("mask_edit".into()));
        }
        if config.max_request_bytes < config.image_limits.max_request_bytes
            || config.max_response_bytes < config.image_limits.max_response_bytes
        {
            return Err(Error::Invalid("IMAGE_TRANSPORT_LIMIT_CONFIGURATION".into()));
        }
        config.max_request_bytes = config.image_limits.max_request_bytes;
        config.max_response_bytes = config.image_limits.max_response_bytes;
        let adapter = Adapter::new(config, Wire::Responses)?;
        let mut edits = reqwest::Url::parse(&adapter.config.endpoint)
            .map_err(|_| Error::Invalid("IMAGE_ENDPOINT".into()))?;
        let prefix = edits
            .path()
            .strip_suffix("/images/generations")
            .ok_or_else(|| Error::Invalid("IMAGE_ENDPOINT".into()))?;
        edits.set_path(&format!("{prefix}/images/edits"));
        Ok(Self {
            adapter,
            edits_endpoint: edits.into(),
        })
    }
    pub fn capabilities(&self) -> &Capabilities {
        &self.adapter.config.capabilities
    }
    fn body(&self, request: &PreparedImageRequest) -> Result<Value> {
        let config = &self.adapter.config;
        let supported = match request.action() {
            ImageAction::Generate => config.capabilities.images.generation,
            ImageAction::Edit => config.capabilities.images.reference_edit,
            ImageAction::MaskEdit => false,
        };
        if !supported || request.mask().is_some() {
            return Err(Error::Unsupported(format!("{:?}", request.action())));
        }
        if request.images().len() > config.image_limits.max_images {
            return Err(Error::Invalid("IMAGE_COUNT_LIMIT".into()));
        }
        if !config
            .image_limits
            .allowed_mime
            .iter()
            .any(|m| m == request.output_format().mime_type())
        {
            return Err(Error::Unsupported("image_output_format".into()));
        }
        let mut body = json!({"model":config.capabilities.limits.model,"prompt":request.prompt(),"n":1,"output_format":request.output_format()});
        if let Some(size) = request.size() {
            body["size"] = json!(size);
        }
        if !request.images().is_empty() {
            let mut images = Vec::new();
            for image in request.images() {
                image.validate(&config.image_limits)?;
                images.push(json!({"image_url":format!("data:{};base64,{}",image.mime_type(),image.data_base64())}));
            }
            body["images"] = json!(images);
        }
        if serde_json::to_vec(&body).map_err(|_| Error::Storage)?.len() > config.max_request_bytes {
            return Err(Error::Invalid("IMAGE_REQUEST_BYTES_LIMIT".into()));
        }
        Ok(body)
    }
}
impl ImagePort for OpenAiImages {
    fn identity(&self) -> String {
        let c = &self.adapter.config;
        digest(&json!([
            "openai-images-1",
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
        self.adapter.config.capabilities.limits.estimate(
            self.adapter.config.input_token_upper_bound,
            self.adapter.config.capabilities.limits.max_output,
        )
    }
    async fn invoke(
        &self,
        context: &ToolContext,
        request: PreparedImageRequest,
    ) -> Result<ImageResponse> {
        let body = self.body(&request)?;
        let endpoint = if request.images().is_empty() {
            &self.adapter.config.endpoint
        } else {
            &self.edits_endpoint
        };
        let parsed = match self.adapter.send_at(context, endpoint, body, &[]).await? {
            HttpResult::Json(parsed) => parsed,
            HttpResult::Failure(ExternalResult::Failed {
                code,
                usage,
                retry_after,
                ..
            }) => {
                return Ok(ImageResponse::Received {
                    images: vec![],
                    usage,
                    failure: Some(code),
                    retry_after,
                });
            }
            _ => return Ok(ImageResponse::Unknown),
        };
        let mut usage = match self.adapter.usage(&parsed) {
            Ok(usage) => usage,
            Err(_) => return Ok(ImageResponse::Unknown),
        };
        let mut failure = None;
        let data = parsed["data"].as_array();
        let count = data.map_or(0, Vec::len);
        let images = if count == 1 {
            match data.unwrap()[0]["b64_json"]
                .as_str()
                .filter(|s| !s.is_empty())
            {
                Some(encoded) => vec![EncodedImage {
                    mime_type: request.output_format().mime_type().into(),
                    data_base64: encoded.into(),
                }],
                None => {
                    failure = Some("IMAGE_FORMAT".into());
                    vec![]
                }
            }
        } else {
            failure = Some("IMAGE_COUNT_LIMIT".into());
            vec![]
        };
        usage
            .quantities
            .insert("image_input_count".into(), request.images().len() as u64);
        usage
            .quantities
            .insert("image_output_count".into(), count as u64);
        for image in &images {
            if let Err(error) = self
                .adapter
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
            retry_after: None,
        })
    }
}

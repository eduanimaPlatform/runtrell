// SPDX-License-Identifier: Apache-2.0
use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use runtrell::{attachments::*, data::*, document_view::*, images::*};
use runtrell_testkit::{FakePolicy, TestClock};
use std::sync::Arc;

async fn send_image<M: ModelPort>(
    inner: M,
    tool: &DocumentView<DataStore, FakePolicy, memory::MemoryStore, TestClock>,
    ctx: &ToolContext,
    images: Vec<ImageReference>,
    encoded: &str,
    mime: &str,
) {
    let model = ImageModel {
        inner,
        resolver: ImageResolver {
            store: tool.resolver.store.clone(),
            policy: FakePolicy::default(),
            accesses: vec![tool.output_access.clone()],
            limits: tool.resolver.limits.clone(),
        },
        runs: tool.runs.clone(),
    };
    let mut req = request();
    req.images = images;
    let prepared = model
        .prepare_request(&tool.runs.get(&ctx.claim.run).await.unwrap(), req)
        .await
        .unwrap();
    assert_eq!(prepared.resolved_images()[0].data_base64(), encoded);
    assert_eq!(prepared.resolved_images()[0].mime_type(), mime);
    model.input_upper_bound(&prepared).unwrap();
    assert!(matches!(
        model.invoke(ctx, prepared).await.unwrap(),
        ExternalResult::Applied { .. }
    ));
}
#[tokio::test]
async fn jpeg_webp_document_views_reach_each_image_adapter_and_invalid_bytes_are_rejected() {
    let kinds = [
        #[cfg(feature = "openai")]
        "openai",
        #[cfg(feature = "gemini")]
        "gemini",
        #[cfg(feature = "openrouter")]
        "openrouter",
    ];
    for kind in kinds {
        for format in ["jpeg", "webp"] {
            let mime = format!("image/{format}");
            let encoded = super::still_images::encoded_output(format);
            let bytes = STANDARD.decode(&encoded).unwrap();
            let runs = Arc::new(memory::MemoryStore::default());
            let account = runs
                .create_account("tenant", Amount::new(10000, 10000), None)
                .await
                .unwrap();
            let handle = runs
                .start(RunSpec::new("tenant", "view", account, "inspect image"))
                .await
                .unwrap();
            let claim = runs.claim(&handle, "worker", 0, 100).await.unwrap();
            let access = Access::owner(Namespace {
                owner: handle.clone(),
                agent: "main".into(),
            });
            let data = DataStore::memory(StoragePolicy::default());
            let file = data.write(&access, "image", None, &bytes).await.unwrap();
            let limits = ImageLimits {
                allowed_mime: vec![mime.clone()],
                ..Default::default()
            };
            let tool = DocumentView {
                resolver: ImageResolver {
                    store: data.clone(),
                    policy: FakePolicy::default(),
                    accesses: vec![access.clone()],
                    limits: limits.clone(),
                },
                external: None,
                runs: runs.clone(),
                clock: TestClock::default(),
                output_access: access.clone(),
                limits: DocumentViewLimits::default(),
            };
            let ctx = ToolContext {
                claim,
                principal: "worker".into(),
                operation: "view".into(),
                attempt: 1,
                deadline: None,
                permission: "document.view".into(),
                account_binding: "documents".into(),
                recovery_proof: None,
            };
            let input = json!({"attachment":Attachment::Stored(ImageReference::Workspace {namespace:access.namespace().clone(),file,mime_type:mime.clone()}),"pages":null});
            let ExternalResult::Applied { output, .. } =
                tool.invoke(&ctx, input.clone()).await.unwrap()
            else {
                panic!()
            };
            assert_eq!(output["page_count"], 1);
            let images: Vec<ImageReference> =
                serde_json::from_value(output["image_inputs"].clone()).unwrap();
            assert_eq!(images[0].hash(), content_digest(&bytes));
            let (endpoint, http) = server(200, "", response(kind, "completed", false)).await;
            let mut c = config(endpoint);
            c.run = handle;
            c.image_limits = limits;
            c.capabilities.images.input = true;
            match kind {
                #[cfg(feature = "openai")]
                "openai" => {
                    send_image(
                        OpenAi::new(c).unwrap(),
                        &tool,
                        &ctx,
                        images,
                        &encoded,
                        &mime,
                    )
                    .await
                }
                #[cfg(feature = "gemini")]
                "gemini" => {
                    send_image(
                        Gemini::new(c).unwrap(),
                        &tool,
                        &ctx,
                        images,
                        &encoded,
                        &mime,
                    )
                    .await
                }
                #[cfg(feature = "openrouter")]
                "openrouter" => {
                    send_image(
                        OpenRouter::new(c).unwrap(),
                        &tool,
                        &ctx,
                        images,
                        &encoded,
                        &mime,
                    )
                    .await
                }
                _ => unreachable!(),
            }
            let sent = http.await.unwrap();
            assert_eq!(sent.len(), 1);
            let wire: Value =
                serde_json::from_str(sent[0].split("\r\n\r\n").nth(1).unwrap()).unwrap();
            let (sent_mime, sent_bytes) = match kind {
                "gemini" => (
                    wire["contents"][1]["parts"][0]["inlineData"]["mimeType"]
                        .as_str()
                        .unwrap()
                        .to_owned(),
                    STANDARD
                        .decode(
                            wire["contents"][1]["parts"][0]["inlineData"]["data"]
                                .as_str()
                                .unwrap(),
                        )
                        .unwrap(),
                ),
                _ => {
                    let url = if kind == "openai" {
                        wire["input"][1]["content"][0]["image_url"]
                            .as_str()
                            .unwrap()
                    } else {
                        wire["messages"][1]["content"][0]["image_url"]["url"]
                            .as_str()
                            .unwrap()
                    };
                    let (meta, b64) = url.split_once(";base64,").unwrap();
                    (
                        meta.strip_prefix("data:").unwrap().to_owned(),
                        STANDARD.decode(b64).unwrap(),
                    )
                }
            };
            assert_eq!(sent_mime, mime);
            assert_eq!(sent_bytes, bytes);
            let invalid = data
                .write(&access, "invalid", None, b"not an image")
                .await
                .unwrap();
            let mut bad_ctx = ctx.clone();
            bad_ctx.operation = "invalid-image".into();
            assert!(
                matches!(tool.invoke(&bad_ctx,json!({"attachment":Attachment::Stored(ImageReference::Workspace {namespace:access.namespace().clone(),file:invalid,mime_type:mime}),"pages":null})).await,Err(Error::Invalid(code)) if code=="IMAGE_FORMAT")
            );
            let mut bad_range = input;
            bad_range["pages"] = json!({"first":2,"last":2});
            bad_ctx.operation = "invalid-range".into();
            assert_eq!(
                tool.invoke(&bad_ctx, bad_range).await.unwrap_err(),
                Error::Invalid("DOCUMENT_PAGE_RANGE".into())
            );
        }
    }
}

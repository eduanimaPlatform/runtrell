// SPDX-License-Identifier: Apache-2.0
use runtrell::images::*;

#[test]
fn image_bounds_and_base64_transport_agree() {
    let limits = ImageLimits::default();
    limits.validate().unwrap();
    assert!(limits.max_request_bytes >= limits.max_image_bytes.div_ceil(3) * 4 * limits.max_images);
    assert!(
        limits.max_response_bytes >= limits.max_image_bytes.div_ceil(3) * 4 * limits.max_images
    );
    assert!(
        ImageLimits {
            max_request_bytes: 1024,
            ..limits
        }
        .validate()
        .is_err()
    );
}

#[test]
fn invalid_png_is_rejected_before_use() {
    assert!(
        ImageLimits::default()
            .inspect("image/png", b"not a png")
            .is_err()
    );
    assert!(
        ImageLimits::default()
            .inspect("image/jpeg", b"not a jpeg")
            .is_err()
    );
}

#[test]
fn output_options_and_opt_in_mime_are_explicit() {
    let input: ImageRequest = serde_json::from_value(serde_json::json!({"prompt":"draw"})).unwrap();
    assert_eq!(input.output_format, ImageOutputFormat::Png);
    for format in ["png", "jpeg", "webp"] {
        let input: ImageRequest = serde_json::from_value(
            serde_json::json!({"prompt":"draw","output_format":format,"size":"1024x1024"}),
        )
        .unwrap();
        assert_eq!(input.output_format.mime_type(), format!("image/{format}"));
    }
    assert!(
        serde_json::from_value::<ImageRequest>(
            serde_json::json!({"prompt":"draw","output_format":"gif"})
        )
        .is_err()
    );
    let limits = ImageLimits {
        allowed_mime: vec!["image/png".into(), "image/jpeg".into(), "image/webp".into()],
        ..ImageLimits::default()
    };
    limits.validate().unwrap();
    for (mime, bytes) in [
        ("image/jpeg", b"\xff\xd8\xff\xd9".as_slice()),
        ("image/webp", b"RIFF\x04\0\0\0WEBP".as_slice()),
    ] {
        assert!(limits.inspect(mime, bytes).is_err());
    }
}

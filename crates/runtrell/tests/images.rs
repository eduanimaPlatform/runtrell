// SPDX-License-Identifier: Apache-2.0
use base64::{Engine, engine::general_purpose::STANDARD};
use runtrell::images::*;

#[test]
fn host_size_policy_rejects_small_edit_and_exposes_constraints() {
    let policy = ImageSizePolicy::Dimensions {
        multiple: 16,
        min_pixels: 655_360,
        max_pixels: 8_294_400,
        max_edge: 3840,
        max_aspect_ratio: 3,
        auto: true,
    };
    policy.validate_configuration().unwrap();
    let error = policy.validate_size(Some("384x384")).unwrap_err();
    assert!(error.to_string().contains("655360"));
    for value in [
        "1024x640",
        "1536x768",
        "1536x512",
        "1024x1024",
        "3840x2160",
        "2160x3840",
        "auto",
    ] {
        policy.validate_size(Some(value)).unwrap();
    }
    for value in [
        "",
        "0x1024",
        "1535x768",
        "3840x384",
        "3840x3840",
        "01536x768",
    ] {
        assert!(policy.validate_size(Some(value)).is_err(), "{value}");
    }
    let schema = policy.schema();
    assert!(schema.to_string().contains("655360"));
}

#[test]
fn finite_and_unsized_size_policies_are_explicit() {
    let values = ImageSizePolicy::Values {
        sizes: vec!["1024x1024".into(), "1536x1024".into()],
        auto: false,
    };
    values.validate_configuration().unwrap();
    assert!(values.validate_size(Some("1024x1024")).is_ok());
    assert!(values.validate_size(Some("384x384")).is_err());
    assert!(values.validate_size(Some("auto")).is_err());
    assert!(values.validate_size(None).is_err());
    assert!(values.validate_size(Some("")).is_err());
    assert!(values.schema().to_string().contains("1536x1024"));
    assert!(ImageSizePolicy::Unsized.validate_size(None).is_ok());
    assert!(
        ImageSizePolicy::Unsized
            .validate_size(Some("auto"))
            .is_err()
    );
}

#[test]
fn svg_codec_is_utf8_xml_with_separate_byte_bound_and_no_pixel_bound() {
    let xml = "<?xml version=\"1.0\"?><svg xmlns=\"http://www.w3.org/2000/svg\" width=\"999999\" height=\"999999\"><text>図</text></svg>";
    let mut limits = ImageLimits {
        allowed_mime: vec!["image/svg+xml".into()],
        max_image_bytes: 1,
        max_pixels: 1,
        max_width: 1,
        max_height: 1,
        max_svg_bytes: xml.len(),
        ..Default::default()
    };
    limits.validate().unwrap();
    assert_eq!(
        limits
            .decode("image/svg+xml", &STANDARD.encode(xml))
            .unwrap(),
        xml.as_bytes()
    );
    limits.max_svg_bytes -= 1;
    assert!(
        limits
            .decode("image/svg+xml", &STANDARD.encode(xml))
            .is_err()
    );
    limits.max_svg_bytes = 4096;
    for invalid in [
        b"not XML".as_slice(),
        b"\xff",
        b"<svg/>",
        b"<svg xmlns=\"http://www.w3.org/2000/svg\"><g></svg>",
        b"<!DOCTYPE svg SYSTEM 'file:///ignored'><svg xmlns=\"http://www.w3.org/2000/svg\"/>",
        b"<svg xmlns=\"http://www.w3.org/2000/svg\"/><svg xmlns=\"http://www.w3.org/2000/svg\"/>",
    ] {
        assert!(
            limits
                .decode("image/svg+xml", &STANDARD.encode(invalid))
                .is_err()
        );
    }
    assert!(limits.decode("image/svg+xml", "???").is_err());
}

#[test]
fn closed_action_types_reject_cross_action_and_unsupported_fields() {
    use serde_json::json;
    assert!(
        serde_json::from_value::<RasterGenerate>(json!({"prompt":"x","references":[]})).is_err()
    );
    assert!(
        serde_json::from_value::<RasterEdit>(
            json!({"prompt":"x","references":[],"output_format":"svg"})
        )
        .is_err()
    );
    assert!(serde_json::from_value::<SvgGenerate>(json!({"prompt":"x","stream":false})).is_err());
    for quality in ["auto", "low", "medium", "high"] {
        assert!(
            serde_json::from_value::<SvgGenerate>(json!({"prompt":"x","quality":quality})).is_ok()
        );
    }
    for quality in ["xhigh", "max"] {
        assert!(
            serde_json::from_value::<SvgGenerate>(json!({"prompt":"x","quality":quality})).is_err()
        );
    }
}

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
    let input: RasterGenerate =
        serde_json::from_value(serde_json::json!({"prompt":"draw"})).unwrap();
    assert_eq!(input.output_format, ImageOutputFormat::Png);
    for format in ["png", "jpeg", "webp"] {
        let input: RasterGenerate = serde_json::from_value(
            serde_json::json!({"prompt":"draw","output_format":format,"size":"1024x1024"}),
        )
        .unwrap();
        assert_eq!(input.output_format.mime_type(), format!("image/{format}"));
    }
    assert!(
        serde_json::from_value::<RasterGenerate>(
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

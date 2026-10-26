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

// SPDX-License-Identifier: Apache-2.0
use runtrell::{attachments::*, data::content_digest, document_view::select_pdf};

#[test]
fn bytes_validate_content_version_and_range() {
    let target = ExternalAttachment {
        reference_id: "file".into(),
        version: "v1".into(),
        pages: Some(PageRange { first: 2, last: 3 }),
        mime_type: "application/pdf".into(),
        bytes: 3,
        digest: content_digest(b"abc"),
    };
    ResolvedAttachment::bytes(target.clone(), b"abc".to_vec())
        .validate(&target)
        .unwrap();
    assert!(
        ResolvedAttachment::bytes(target.clone(), b"abd".to_vec())
            .validate(&target)
            .is_err()
    );
    let mut changed = target.clone();
    changed.version = "v2".into();
    assert!(
        ResolvedAttachment::url(changed, "https://example.test/one")
            .validate(&target)
            .is_err()
    );
    for url in ["https://example.test/one", "https://example.test/two"] {
        ResolvedAttachment::url(target.clone(), url)
            .validate(&target)
            .unwrap();
    }
}
#[test]
fn pdf_rejects_malformed_data_and_invalid_ranges() {
    assert!(select_pdf(b"not pdf", Some(&PageRange { first: 1, last: 1 }), 10, 1024).is_err());
    assert!(PageRange { first: 0, last: 1 }.validate().is_err());
    assert!(PageRange { first: 2, last: 1 }.validate().is_err());
}

#[test]
fn lookup_identity_cannot_silently_send_without_declared_local_payload() {
    use runtrell::{data::*, images::*, *};
    let reference = ImageReference::Workspace {
        namespace: Namespace {
            owner: RunHandle {
                tenant: "tenant".into(),
                id: "run".into(),
            },
            agent: "main".into(),
        },
        file: FileVersion {
            path: "input.pdf".into(),
            revision: 1,
            hash: content_digest(b"abc"),
            bytes: 3,
        },
        mime_type: "application/pdf".into(),
    };
    let request = ModelRequest {
        model: "model".into(),
        messages: vec![],
        tools: vec![],
        max_output: 10,
        metadata: serde_json::Value::Null,
        images: vec![],
        attachments: vec![Attachment::Stored(reference)],
    };
    let lookup = PreparedModelRequest::for_lookup(request).unwrap();
    assert_eq!(
        lookup.validate_for_send().unwrap_err(),
        Error::Invalid("MODEL_INPUT_NOT_PREPARED".into())
    );
}

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
fn selected_pdf_preserves_visible_annotations_and_form_appearance_without_actions() {
    use lopdf::{Document, Object, Stream, dictionary};
    let mut doc = Document::with_version("1.7");
    let tree = doc.new_object_id();
    let font =
        doc.add_object(dictionary! {"Type"=>"Font","Subtype"=>"Type1","BaseFont"=>"Helvetica"});
    let resources = dictionary! {"Font"=>dictionary! {"F1"=>font,"A"=>font,"AA"=>font}};
    let mut annots: Vec<Object> = Vec::new();
    for (kind, visible) in [
        ("FreeText", "NOTE"),
        ("Highlight", "MARK"),
        ("Ink", "HAND"),
        ("Stamp", "STAMP"),
        ("Widget", "VALUE"),
    ] {
        let ap = doc.add_object(Stream::new(dictionary! {"Type"=>"XObject","Subtype"=>"Form","BBox"=>vec![0.into(),0.into(),100.into(),20.into()],"Resources"=>resources.clone()}, format!("BT /F1 10 Tf ({visible}) Tj ET").into_bytes()));
        let mut annot = dictionary! {"Type"=>"Annot","Subtype"=>kind,"Rect"=>vec![0.into(),0.into(),100.into(),20.into()],"AP"=>dictionary! {"N"=>ap},"AA"=>dictionary! {"E"=>dictionary! {"S"=>"JavaScript","JS"=>Object::string_literal("evil()")}}};
        if kind == "Widget" {
            annot.set("FT", Object::Name(b"Tx".to_vec()));
            annot.set("T", Object::string_literal("field"));
            annot.set("V", Object::string_literal("VALUE"));
        }
        annots.push(doc.add_object(annot).into());
    }
    let widget = annots.last().unwrap().clone();
    let removed = doc.add_object(dictionary! {"Type"=>"Annot","Subtype"=>"Widget","FT"=>"Tx","T"=>Object::string_literal("REMOVED_FIELD"),"V"=>Object::string_literal("REMOVED_VALUE")});
    let field = doc.add_object(dictionary! {"FT"=>"Tx","T"=>Object::string_literal("parent"),"V"=>Object::string_literal("VALUE"),"Kids"=>vec![widget.clone(),removed.into()]});
    doc.get_dictionary_mut(widget.as_reference().unwrap())
        .unwrap()
        .set("Parent", field);
    doc.get_dictionary_mut(removed)
        .unwrap()
        .set("Parent", field);
    annots.push(doc.add_object(dictionary! {"Type"=>"Annot","Subtype"=>"Link","Rect"=>vec![0.into(),0.into(),100.into(),20.into()],"A"=>dictionary! {"S"=>"URI","URI"=>Object::string_literal("https://evil.test")}}).into());
    let content = doc.add_object(Stream::new(dictionary! {}, b"q Q".to_vec()));
    let page = doc.add_object(dictionary! {"Type"=>"Page","Parent"=>tree,"MediaBox"=>vec![0.into(),0.into(),200.into(),200.into()],"Contents"=>content,"Annots"=>annots,"AA"=>dictionary! {"O"=>dictionary! {"S"=>"JavaScript","JS"=>Object::string_literal("evil()")}}});
    let other_page = doc.add_object(dictionary! {"Type"=>"Page","Parent"=>tree,"MediaBox"=>vec![0.into(),0.into(),200.into(),200.into()],"Annots"=>vec![removed.into()]});
    doc.objects.insert(
        tree,
        dictionary! {"Type"=>"Pages","Kids"=>vec![page.into(),other_page.into()],"Count"=>2}.into(),
    );
    let catalog = doc.add_object(dictionary! {"Type"=>"Catalog","Pages"=>tree,"AcroForm"=>dictionary! {"Fields"=>vec![field.into()],"CO"=>vec![removed.into()],"DR"=>resources,"DA"=>Object::string_literal("/F1 10 Tf 0 g"),"NeedAppearances"=>true},"OpenAction"=>dictionary! {"S"=>"JavaScript","JS"=>Object::string_literal("evil()")}});
    doc.trailer.set("Root", catalog);
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();
    let (_, selected) =
        select_pdf(&bytes, Some(&PageRange { first: 1, last: 1 }), 4, 100_000).unwrap();
    let selected = Document::load_mem(&selected.unwrap()).unwrap();
    assert!(!selected.objects.contains_key(&removed));
    assert!(!selected.objects.contains_key(&other_page));
    assert_eq!(
        selected
            .get_dictionary(field)
            .unwrap()
            .get(b"Kids")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let page = selected
        .get_dictionary(*selected.get_pages().values().next().unwrap())
        .unwrap();
    let annotations = page.get(b"Annots").unwrap().as_array().unwrap();
    assert_eq!(annotations.len(), 6);
    for annotation in &annotations[..5] {
        let annotation = selected
            .get_dictionary(annotation.as_reference().unwrap())
            .unwrap();
        assert!(!annotation.has(b"A") && !annotation.has(b"AA"));
        let ap = annotation
            .get(b"AP")
            .unwrap()
            .as_dict()
            .unwrap()
            .get(b"N")
            .unwrap()
            .as_reference()
            .unwrap();
        assert!(
            !selected
                .get_object(ap)
                .unwrap()
                .as_stream()
                .unwrap()
                .content
                .is_empty()
        );
    }
    let form = selected
        .catalog()
        .unwrap()
        .get(b"AcroForm")
        .unwrap()
        .as_dict()
        .unwrap();
    assert!(form.has(b"DR") && form.has(b"DA") && form.has(b"NeedAppearances"));
    assert_eq!(form.get(b"Fields").unwrap().as_array().unwrap().len(), 1);
    let fonts = form
        .get(b"DR")
        .unwrap()
        .as_dict()
        .unwrap()
        .get(b"Font")
        .unwrap()
        .as_dict()
        .unwrap();
    assert!(fonts.has(b"A") && fonts.has(b"AA"));
    fn inert(object: &Object) {
        match object {
            Object::Dictionary(d) => {
                for (key, value) in d {
                    // Resource names can legitimately be A/AA.
                    assert!(
                        ![b"URI".as_slice(), b"JS", b"JavaScript", b"OpenAction"]
                            .contains(&key.as_slice()),
                        "active key {key:?}"
                    );
                    inert(value);
                }
            }
            Object::Stream(s) => inert(&Object::Dictionary(s.dict.clone())),
            Object::Array(a) => {
                for v in a {
                    inert(v);
                }
            }
            _ => (),
        }
    }
    for object in selected.objects.values() {
        inert(object);
    }
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

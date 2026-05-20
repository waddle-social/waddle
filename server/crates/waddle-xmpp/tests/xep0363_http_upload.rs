//! XEP-0363: HTTP File Upload — dedicated conformance suite.
//!
//! Pins the audit-level invariants:
//!
//! - §3 namespace string `urn:xmpp:http:upload:0`,
//! - §"Discovering Support" advertisement on `server_features()`
//!   AND on the upload-service component,
//! - §3.1 request shape: `<iq type='get'><request xmlns=… filename=…
//!   size=… content-type=…/></iq>`,
//! - §3.2 slot response shape: `<slot><put url=…/><get url=…/></slot>`
//!   plus optional `<header>` children on `<put>`,
//! - §3.3 error responses: `<not-acceptable/>` + `<file-too-large>`
//!   for FileTooLarge, `<resource-constraint/>` + `<retry/>` for
//!   QuotaReached, `<forbidden/>` for NotAllowed.

use minidom::Element;
use waddle_xmpp::disco::{server_features, upload_service_features, Feature};
use waddle_xmpp::xep::xep0363::{
    build_upload_error, build_upload_slot_response, effective_content_type, is_upload_request,
    parse_upload_request, sanitize_filename, UploadError, UploadRequest, UploadSlot,
    DEFAULT_MAX_FILE_SIZE, NS_HTTP_UPLOAD,
};
use xmpp_parsers::iq::Iq;

// ── §3 namespace ─────────────────────────────────────────────────────

#[test]
fn xep0363_namespace_matches_spec() {
    assert_eq!(NS_HTTP_UPLOAD, "urn:xmpp:http:upload:0");
}

// ── §"Discovering Support" advertisement ────────────────────────────

#[test]
fn xep0363_server_features_advertise_http_upload() {
    let feats = server_features();
    let target = Feature::http_upload();
    assert_eq!(target.0, NS_HTTP_UPLOAD);
    assert!(
        feats.iter().any(|f| f == &target),
        "server_features() must advertise `urn:xmpp:http:upload:0`"
    );
}

#[test]
fn xep0363_upload_service_features_include_http_upload() {
    // The upload service is its own disco-info component
    // (`upload.<domain>`). The §"Discovering Support" advert
    // ALSO appears there so clients dispatching against the
    // component get the same answer.
    let feats = upload_service_features();
    assert!(
        feats.iter().any(|f| f.0 == NS_HTTP_UPLOAD),
        "upload service disco MUST advertise the feature on its own component too"
    );
}

// ── §3.1 request shape (parsing) ────────────────────────────────────

fn upload_request_iq(filename: &str, size: u64, content_type: Option<&str>) -> Iq {
    let mut req = Element::builder("request", NS_HTTP_UPLOAD)
        .attr(minidom::rxml::xml_ncname!("filename").to_owned(), filename)
        .attr(
            minidom::rxml::xml_ncname!("size").to_owned(),
            size.to_string(),
        );
    if let Some(ct) = content_type {
        req = req.attr(minidom::rxml::xml_ncname!("content-type").to_owned(), ct);
    }
    Iq::Get {
        from: None,
        to: None,
        id: "u-1".into(),
        payload: req.build(),
    }
}

#[test]
fn xep0363_classifier_accepts_iq_get_with_request_payload() {
    // §3.1: client → upload-service `iq/type='get'` carrying a
    // namespaced `<request/>` payload.
    let iq = upload_request_iq("file.bin", 123, Some("application/octet-stream"));
    assert!(is_upload_request(&iq));
}

#[test]
fn xep0363_classifier_rejects_iq_set_with_request_payload() {
    // §3.1 fixes the verb as `get`. A `set` carrying the same
    // payload is malformed and MUST NOT be classified as a
    // request — otherwise an attacker could probe the upload
    // grant path with a request shape the spec doesn't define.
    let iq = Iq::Set {
        from: None,
        to: None,
        id: "u-1".into(),
        payload: Element::builder("request", NS_HTTP_UPLOAD)
            .attr(minidom::rxml::xml_ncname!("filename").to_owned(), "x")
            .attr(minidom::rxml::xml_ncname!("size").to_owned(), "1")
            .build(),
    };
    assert!(!is_upload_request(&iq));
}

#[test]
fn xep0363_parse_request_round_trips_filename_size_content_type() {
    let iq = upload_request_iq("photo.jpg", 4096, Some("image/jpeg"));
    let req = parse_upload_request(&iq).expect("valid request");
    assert_eq!(req.filename, "photo.jpg");
    assert_eq!(req.size, 4096);
    assert_eq!(req.content_type.as_deref(), Some("image/jpeg"));
}

#[test]
fn xep0363_parse_request_rejects_missing_filename() {
    let iq = Iq::Get {
        from: None,
        to: None,
        id: "u-1".into(),
        payload: Element::builder("request", NS_HTTP_UPLOAD)
            .attr(minidom::rxml::xml_ncname!("size").to_owned(), "10")
            .build(),
    };
    let err = parse_upload_request(&iq).expect_err("missing filename");
    assert!(matches!(err, UploadError::BadRequest(_)));
}

#[test]
fn xep0363_parse_request_rejects_missing_size() {
    let iq = Iq::Get {
        from: None,
        to: None,
        id: "u-1".into(),
        payload: Element::builder("request", NS_HTTP_UPLOAD)
            .attr(minidom::rxml::xml_ncname!("filename").to_owned(), "x")
            .build(),
    };
    let err = parse_upload_request(&iq).expect_err("missing size");
    assert!(matches!(err, UploadError::BadRequest(_)));
}

#[test]
fn xep0363_parse_request_rejects_zero_size_and_non_numeric_size() {
    let zero = Iq::Get {
        from: None,
        to: None,
        id: "u-1".into(),
        payload: Element::builder("request", NS_HTTP_UPLOAD)
            .attr(minidom::rxml::xml_ncname!("filename").to_owned(), "x")
            .attr(minidom::rxml::xml_ncname!("size").to_owned(), "0")
            .build(),
    };
    assert!(matches!(
        parse_upload_request(&zero),
        Err(UploadError::BadRequest(_))
    ));

    let bogus = Iq::Get {
        from: None,
        to: None,
        id: "u-1".into(),
        payload: Element::builder("request", NS_HTTP_UPLOAD)
            .attr(minidom::rxml::xml_ncname!("filename").to_owned(), "x")
            .attr(minidom::rxml::xml_ncname!("size").to_owned(), "huge")
            .build(),
    };
    assert!(matches!(
        parse_upload_request(&bogus),
        Err(UploadError::BadRequest(_))
    ));
}

// ── §3.2 slot-response shape ────────────────────────────────────────

#[test]
fn xep0363_slot_response_emits_put_get_and_optional_headers() {
    // §3.2 example: `<slot xmlns=…><put url=…><header
    // name='Authorization'>Bearer …</header></put><get url=…/></slot>`.
    let original = upload_request_iq("file.bin", 100, None);
    let slot = UploadSlot {
        put_url: "https://upload.example/abc/file.bin".into(),
        put_headers: vec![
            ("Authorization".into(), "Bearer abc".into()),
            ("Cookie".into(), "foo=bar".into()),
        ],
        get_url: "https://cdn.example/abc/file.bin".into(),
    };
    let response = build_upload_slot_response(&original, &slot);
    let Iq::Result {
        payload: Some(slot_elem),
        ..
    } = response
    else {
        panic!("response must be iq type='result' with payload");
    };

    assert_eq!(slot_elem.name(), "slot");
    assert_eq!(slot_elem.ns(), NS_HTTP_UPLOAD);

    let put = slot_elem
        .children()
        .find(|c| c.name() == "put" && c.ns() == NS_HTTP_UPLOAD)
        .expect("<put> present");
    assert_eq!(put.attr("url"), Some("https://upload.example/abc/file.bin"));

    let headers: Vec<_> = put
        .children()
        .filter(|c| c.name() == "header" && c.ns() == NS_HTTP_UPLOAD)
        .map(|c| (c.attr("name").unwrap_or(""), c.text()))
        .collect();
    assert_eq!(headers.len(), 2);
    assert_eq!(headers[0], ("Authorization", "Bearer abc".to_owned()));
    assert_eq!(headers[1], ("Cookie", "foo=bar".to_owned()));

    let get = slot_elem
        .children()
        .find(|c| c.name() == "get" && c.ns() == NS_HTTP_UPLOAD)
        .expect("<get> present");
    assert_eq!(get.attr("url"), Some("https://cdn.example/abc/file.bin"));
}

// ── §3.3 error-response shapes ──────────────────────────────────────

#[test]
fn xep0363_file_too_large_error_carries_max_file_size_child() {
    // §3.3 FileTooLarge: `<not-acceptable/>` plus a
    // `<file-too-large xmlns='urn:xmpp:http:upload:0'>
    //   <max-file-size>BYTES</max-file-size>
    // </file-too-large>` app-error child so the client knows the
    // server-enforced limit.
    let xml = build_upload_error(
        "err-too-big",
        &UploadError::FileTooLarge {
            max_size: 10_485_760,
        },
    );

    assert!(xml.contains("<not-acceptable"));
    assert!(xml.contains("<file-too-large"));
    assert!(xml.contains(NS_HTTP_UPLOAD));
    assert!(xml.contains("<max-file-size>10485760</max-file-size>"));
    assert!(xml.contains("id='err-too-big'"));
}

#[test]
fn xep0363_quota_reached_error_carries_retry_child() {
    // §3.3 QuotaReached: `<resource-constraint/>` plus
    // `<retry xmlns='urn:xmpp:http:upload:0'/>` so the client
    // knows the failure is transient and a retry will eventually
    // succeed.
    let xml = build_upload_error("err-quota", &UploadError::QuotaReached);
    assert!(xml.contains("<resource-constraint"));
    assert!(xml.contains("<retry"));
    assert!(xml.contains(NS_HTTP_UPLOAD));
}

#[test]
fn xep0363_not_allowed_error_uses_forbidden_condition() {
    // §3.3 NotAllowed: `<forbidden/>` with no app-error child —
    // there's nothing extra the client can do to recover.
    let xml = build_upload_error("err-forbidden", &UploadError::NotAllowed);
    assert!(xml.contains("<forbidden"));
    assert!(
        !xml.contains("<retry") && !xml.contains("<file-too-large"),
        "NotAllowed MUST NOT carry app-error children (would imply recovery)"
    );
}

#[test]
fn xep0363_bad_request_and_internal_error_use_xmpp_stanza_conditions() {
    let bad = build_upload_error("err-bad", &UploadError::BadRequest("nope".into()));
    assert!(bad.contains("<bad-request"));

    let internal = build_upload_error("err-internal", &UploadError::InternalError("crash".into()));
    assert!(internal.contains("<internal-server-error"));
}

#[test]
fn xep0363_error_xml_is_well_formed_minidom_serialisation() {
    // The error builder must produce parseable XML — round-trip
    // through minidom proves there's no manual-format!
    // string-concat lurking that could emit broken markup.
    for variant in [
        UploadError::FileTooLarge { max_size: 1024 },
        UploadError::NotAllowed,
        UploadError::QuotaReached,
        UploadError::BadRequest("nope".into()),
        UploadError::InternalError("boom".into()),
    ] {
        let xml = build_upload_error("err", &variant);
        let parsed = xml.parse::<Element>().expect("well-formed XML");
        assert_eq!(parsed.name(), "iq");
        assert_eq!(parsed.attr("type"), Some("error"));
        assert_eq!(parsed.attr("id"), Some("err"));
    }
}

// ── Filename sanitisation + content-type defaults ───────────────────

#[test]
fn xep0363_sanitize_filename_strips_path_components() {
    // Defence: a malicious client uploading `../../etc/passwd`
    // must land at a flat `..._.._.._etc_passwd`-style name —
    // path traversal characters are replaced. The exact
    // replacement table is the implementation's choice; the
    // CONTRACT is "no `/` or `\` survive."
    let sanitized = sanitize_filename("../../etc/passwd");
    assert!(!sanitized.contains('/'));
    assert!(!sanitized.contains('\\'));
}

#[test]
fn xep0363_sanitize_filename_replaces_problematic_chars() {
    let sanitized = sanitize_filename("evil file<>.txt");
    assert!(!sanitized.contains('<'));
    assert!(!sanitized.contains('>'));
    assert!(!sanitized.contains(' '));
    // alphanumerics + . - _ pass through; everything else becomes `_`.
    assert!(sanitized.contains("evil"));
    assert!(sanitized.contains(".txt"));
}

#[test]
fn xep0363_sanitize_filename_caps_length_at_255() {
    let long = "a".repeat(500);
    let sanitized = sanitize_filename(&long);
    assert!(sanitized.len() <= 255);
}

#[test]
fn xep0363_sanitize_filename_substitutes_empty_or_dot_only() {
    assert_eq!(sanitize_filename(""), "file");
    assert_eq!(sanitize_filename("."), "file");
    assert_eq!(sanitize_filename(".."), "file");
}

#[test]
fn xep0363_effective_content_type_defaults_to_octet_stream() {
    // Per §3.1 the `content-type` attribute is optional. When
    // absent, the spec recommends `application/octet-stream` —
    // matches HTTP/1.1's default for unknown bodies.
    assert_eq!(effective_content_type(None), "application/octet-stream");
    assert_eq!(effective_content_type(Some("image/png")), "image/png");
}

// ── Default max-file-size constant ──────────────────────────────────

#[test]
fn xep0363_default_max_file_size_is_a_reasonable_default() {
    // 10MiB default. Test pins the choice so a future change
    // surfaces as a deliberate decision rather than a silent
    // tweak that might enable file-too-large attacks against
    // backends with smaller real limits.
    assert_eq!(DEFAULT_MAX_FILE_SIZE, 10 * 1024 * 1024);
}

// ── Empty UploadRequest builder ─────────────────────────────────────

#[test]
fn xep0363_upload_request_struct_carries_required_and_optional_fields() {
    let req = UploadRequest {
        filename: "a.bin".into(),
        size: 1024,
        content_type: None,
    };
    assert_eq!(req.filename, "a.bin");
    assert_eq!(req.size, 1024);
    assert!(req.content_type.is_none());
}

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
//!   for FileTooLarge, `<resource-constraint/>` + `<retry stamp='…'/>`
//!   for QuotaReached, `<forbidden/>` for NotAllowed.

use chrono::{DateTime, FixedOffset};
use minidom::Element;
use std::str::FromStr;
use waddle_xmpp::disco::{server_features, upload_service_features, Feature};
use waddle_xmpp::xep::xep0363::{
    build_upload_error, build_upload_slot_response, effective_content_type, is_upload_request,
    parse_upload_request, sanitize_filename, UploadBadRequest, UploadError, UploadRequest,
    UploadSlot, DEFAULT_MAX_FILE_SIZE, NS_HTTP_UPLOAD,
};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

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

fn assert_upload_error_iq(
    iq: Iq,
    expected_type: ErrorType,
    expected_condition: DefinedCondition,
    expected_id: &str,
) -> (StanzaError, Element) {
    let Iq::Error {
        id, error, payload, ..
    } = iq
    else {
        panic!("expected upload error IQ");
    };

    assert_eq!(id, expected_id);
    assert_eq!(error.type_, expected_type);
    assert_eq!(error.defined_condition, expected_condition);

    let payload = payload.expect("error IQ carries original request payload");
    assert_eq!(payload.name(), "request");
    assert_eq!(payload.ns(), NS_HTTP_UPLOAD);
    (error, payload)
}

fn fixed_retry_at() -> chrono::DateTime<chrono::Utc> {
    DateTime::parse_from_rfc3339("2026-05-31T12:34:56Z")
        .expect("fixed retry timestamp")
        .with_timezone(&chrono::Utc)
}

fn serialized_iq_element(iq: Iq) -> Element {
    let serialized = String::from(&waddle_xmpp::Stanza::Iq(Box::new(iq)).to_element());
    serialized.parse::<Element>().expect("well-formed XML")
}

#[test]
fn xep0363_file_too_large_error_carries_max_file_size_child() {
    // §3.3 FileTooLarge: `<not-acceptable/>` plus a
    // `<file-too-large xmlns='urn:xmpp:http:upload:0'>
    //   <max-file-size>BYTES</max-file-size>
    // </file-too-large>` app-error child so the client knows the
    // server-enforced limit.
    let request_iq = upload_request_iq("too-big.jpg", 20_000_000, Some("image/jpeg"));
    let (error, payload) = assert_upload_error_iq(
        build_upload_error(
            &request_iq,
            &UploadError::FileTooLarge {
                max_size: 10_485_760,
            },
        ),
        ErrorType::Modify,
        DefinedCondition::NotAcceptable,
        "u-1",
    );

    assert_eq!(payload.attr("filename"), Some("too-big.jpg"));
    assert_eq!(payload.attr("size"), Some("20000000"));
    assert_eq!(payload.attr("content-type"), Some("image/jpeg"));

    let app_error = error.other.expect("file-too-large app error");
    assert_eq!(app_error.name(), "file-too-large");
    assert_eq!(app_error.ns(), NS_HTTP_UPLOAD);
    let max_file_size = app_error
        .get_child("max-file-size", NS_HTTP_UPLOAD)
        .expect("max-file-size child");
    assert_eq!(max_file_size.text(), "10485760");
    assert_eq!(
        app_error
            .children()
            .filter(|child| child.name() == "max-file-size" && child.ns() == NS_HTTP_UPLOAD)
            .count(),
        1
    );
}

#[test]
fn xep0363_upload_error_swaps_request_addresses() {
    let request_iq = Iq::Get {
        from: Some(jid::Jid::from_str("romeo@example.test/garden").expect("from JID")),
        to: Some(jid::Jid::from_str("upload.example.test").expect("to JID")),
        id: "addr-1".into(),
        payload: Element::builder("request", NS_HTTP_UPLOAD)
            .attr(minidom::rxml::xml_ncname!("filename").to_owned(), "a.jpg")
            .attr(minidom::rxml::xml_ncname!("size").to_owned(), "100")
            .build(),
    };

    let Iq::Error { from, to, id, .. } = build_upload_error(&request_iq, &UploadError::NotAllowed)
    else {
        panic!("expected upload error IQ");
    };

    assert_eq!(id, "addr-1");
    assert_eq!(
        from.as_ref().map(ToString::to_string).as_deref(),
        Some("upload.example.test")
    );
    assert_eq!(
        to.as_ref().map(ToString::to_string).as_deref(),
        Some("romeo@example.test/garden")
    );
}

#[test]
fn xep0363_quota_reached_error_carries_retry_child() {
    // §3.3 QuotaReached: `<resource-constraint/>` plus
    // `<retry xmlns='urn:xmpp:http:upload:0' stamp='...'/>` so
    // the client knows the failure is transient and when it may try
    // again.
    let (error, _) = assert_upload_error_iq(
        build_upload_error(
            &upload_request_iq("quota.jpg", 100, None),
            &UploadError::QuotaReached {
                retry_at: fixed_retry_at(),
            },
        ),
        ErrorType::Wait,
        DefinedCondition::ResourceConstraint,
        "u-1",
    );
    let retry = error.other.expect("retry app error");
    assert_eq!(retry.name(), "retry");
    assert_eq!(retry.ns(), NS_HTTP_UPLOAD);
    let stamp = retry.attr("stamp").expect("retry stamp");
    assert_eq!(stamp, "2026-05-31T12:34:56Z");
    assert!(stamp.ends_with('Z'), "retry stamp must be UTC: {stamp}");
    let parsed: DateTime<FixedOffset> =
        DateTime::parse_from_rfc3339(stamp).expect("retry stamp is XEP-0082/RFC3339 date-time");
    assert_eq!(parsed.offset().local_minus_utc(), 0);
}

#[test]
fn xep0363_file_too_large_app_error_survives_serialization() {
    let parsed = serialized_iq_element(build_upload_error(
        &upload_request_iq("serialise<&>.jpg", 20_000_000, None),
        &UploadError::FileTooLarge {
            max_size: 10_485_760,
        },
    ));
    let error = parsed
        .get_child("error", "jabber:client")
        .expect("error child");
    let app_error = error
        .get_child("file-too-large", NS_HTTP_UPLOAD)
        .expect("serialized file-too-large app error");
    assert_eq!(
        app_error
            .get_child("max-file-size", NS_HTTP_UPLOAD)
            .expect("serialized max-file-size")
            .text(),
        "10485760"
    );
    let request = parsed
        .get_child("request", NS_HTTP_UPLOAD)
        .expect("serialized original request");
    assert_eq!(request.attr("filename"), Some("serialise<&>.jpg"));
}

#[test]
fn xep0363_quota_retry_app_error_survives_serialization() {
    let parsed = serialized_iq_element(build_upload_error(
        &upload_request_iq("quota.jpg", 100, None),
        &UploadError::QuotaReached {
            retry_at: fixed_retry_at(),
        },
    ));
    let error = parsed
        .get_child("error", "jabber:client")
        .expect("error child");
    let retry = error
        .get_child("retry", NS_HTTP_UPLOAD)
        .expect("serialized retry app error");
    assert_eq!(retry.attr("stamp"), Some("2026-05-31T12:34:56Z"));
}

#[test]
fn xep0363_not_allowed_error_uses_forbidden_condition() {
    // §3.3 NotAllowed: `<forbidden/>` with no app-error child —
    // there's nothing extra the client can do to recover.
    let (error, _) = assert_upload_error_iq(
        build_upload_error(
            &upload_request_iq("blocked.jpg", 100, None),
            &UploadError::NotAllowed,
        ),
        ErrorType::Auth,
        DefinedCondition::Forbidden,
        "u-1",
    );
    assert!(error.other.is_none());
}

#[test]
fn xep0363_bad_request_and_internal_error_use_xmpp_stanza_conditions() {
    let (bad, _) = assert_upload_error_iq(
        build_upload_error(
            &upload_request_iq("bad.jpg", 100, None),
            &UploadError::BadRequest(UploadBadRequest::InvalidSize),
        ),
        ErrorType::Modify,
        DefinedCondition::BadRequest,
        "u-1",
    );
    assert!(bad.other.is_none());

    let (internal, _) = assert_upload_error_iq(
        build_upload_error(
            &upload_request_iq("internal.jpg", 100, None),
            &UploadError::InternalError,
        ),
        ErrorType::Wait,
        DefinedCondition::InternalServerError,
        "u-1",
    );
    assert!(internal.other.is_none());
}

#[test]
fn xep0363_error_serialization_escapes_text_and_stays_well_formed() {
    let parsed = serialized_iq_element(build_upload_error(
        &upload_request_iq("bad <name> & \"type\".jpg", 100, None),
        &UploadError::BadRequest(UploadBadRequest::InvalidSize),
    ));

    assert_eq!(parsed.name(), "iq");
    assert_eq!(parsed.attr("type"), Some("error"));
    let error = parsed
        .get_child("error", "jabber:client")
        .expect("error child");
    let text = error
        .get_child("text", "urn:ietf:params:xml:ns:xmpp-stanzas")
        .expect("error text");
    assert_eq!(text.text(), "Bad request: invalid size attribute");
    let request = parsed
        .get_child("request", NS_HTTP_UPLOAD)
        .expect("original request");
    assert_eq!(request.attr("filename"), Some("bad <name> & \"type\".jpg"));
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

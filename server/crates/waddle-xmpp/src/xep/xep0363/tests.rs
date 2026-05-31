use super::*;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType};

fn upload_request_iq(id: &str, filename: &str, size: u64) -> Iq {
    Iq::Get {
        from: None,
        to: None,
        id: id.to_string(),
        payload: Element::builder("request", NS_HTTP_UPLOAD)
            .attr(minidom::rxml::xml_ncname!("filename").to_owned(), filename)
            .attr(
                minidom::rxml::xml_ncname!("size").to_owned(),
                size.to_string(),
            )
            .build(),
    }
}

fn fixed_retry_at() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339("2026-05-31T12:34:56Z")
        .expect("fixed retry timestamp")
        .with_timezone(&chrono::Utc)
}

#[test]
fn test_is_upload_request() {
    let request_elem = Element::builder("request", NS_HTTP_UPLOAD)
        .attr(
            minidom::rxml::xml_ncname!("filename").to_owned(),
            "test.jpg",
        )
        .attr(minidom::rxml::xml_ncname!("size").to_owned(), "12345")
        .build();
    let iq = Iq::Get {
        from: None,
        to: None,
        id: "upload-1".to_string(),
        payload: request_elem,
    };

    assert!(is_upload_request(&iq));
}

#[test]
fn test_is_not_upload_request_wrong_ns() {
    let elem = Element::builder("request", "wrong:namespace")
        .attr(
            minidom::rxml::xml_ncname!("filename").to_owned(),
            "test.jpg",
        )
        .attr(minidom::rxml::xml_ncname!("size").to_owned(), "12345")
        .build();
    let iq = Iq::Get {
        from: None,
        to: None,
        id: "test-1".to_string(),
        payload: elem,
    };

    assert!(!is_upload_request(&iq));
}

#[test]
fn test_is_not_upload_request_wrong_type() {
    let elem = Element::builder("request", NS_HTTP_UPLOAD)
        .attr(
            minidom::rxml::xml_ncname!("filename").to_owned(),
            "test.jpg",
        )
        .attr(minidom::rxml::xml_ncname!("size").to_owned(), "12345")
        .build();
    let iq = Iq::Set {
        from: None,
        to: None,
        id: "test-2".to_string(),
        payload: elem,
    };

    assert!(!is_upload_request(&iq));
}

#[test]
fn test_parse_upload_request_full() {
    let request_elem = Element::builder("request", NS_HTTP_UPLOAD)
        .attr(
            minidom::rxml::xml_ncname!("filename").to_owned(),
            "vacation.jpg",
        )
        .attr(minidom::rxml::xml_ncname!("size").to_owned(), "23456")
        .attr(
            minidom::rxml::xml_ncname!("content-type").to_owned(),
            "image/jpeg",
        )
        .build();
    let iq = Iq::Get {
        from: Some("user@example.com".parse().unwrap()),
        to: Some("upload.example.com".parse().unwrap()),
        id: "upload-1".to_string(),
        payload: request_elem,
    };

    let request = parse_upload_request(&iq).unwrap();

    assert_eq!(request.filename, "vacation.jpg");
    assert_eq!(request.size, 23456);
    assert_eq!(request.content_type, Some("image/jpeg".to_string()));
}

#[test]
fn test_parse_upload_request_minimal() {
    let request_elem = Element::builder("request", NS_HTTP_UPLOAD)
        .attr(
            minidom::rxml::xml_ncname!("filename").to_owned(),
            "file.bin",
        )
        .attr(minidom::rxml::xml_ncname!("size").to_owned(), "100")
        .build();
    let iq = Iq::Get {
        from: None,
        to: None,
        id: "upload-2".to_string(),
        payload: request_elem,
    };

    let request = parse_upload_request(&iq).unwrap();

    assert_eq!(request.filename, "file.bin");
    assert_eq!(request.size, 100);
    assert!(request.content_type.is_none());
}

#[test]
fn test_parse_upload_request_missing_filename() {
    let request_elem = Element::builder("request", NS_HTTP_UPLOAD)
        .attr(minidom::rxml::xml_ncname!("size").to_owned(), "100")
        .build();
    let iq = Iq::Get {
        from: None,
        to: None,
        id: "upload-3".to_string(),
        payload: request_elem,
    };

    let result = parse_upload_request(&iq);
    assert!(matches!(result, Err(UploadError::BadRequest(_))));
}

#[test]
fn test_parse_upload_request_missing_size() {
    let request_elem = Element::builder("request", NS_HTTP_UPLOAD)
        .attr(
            minidom::rxml::xml_ncname!("filename").to_owned(),
            "test.txt",
        )
        .build();
    let iq = Iq::Get {
        from: None,
        to: None,
        id: "upload-4".to_string(),
        payload: request_elem,
    };

    let result = parse_upload_request(&iq);
    assert!(matches!(result, Err(UploadError::BadRequest(_))));
}

#[test]
fn test_parse_upload_request_invalid_size() {
    let request_elem = Element::builder("request", NS_HTTP_UPLOAD)
        .attr(
            minidom::rxml::xml_ncname!("filename").to_owned(),
            "test.txt",
        )
        .attr(
            minidom::rxml::xml_ncname!("size").to_owned(),
            "not-a-number",
        )
        .build();
    let iq = Iq::Get {
        from: None,
        to: None,
        id: "upload-5".to_string(),
        payload: request_elem,
    };

    let result = parse_upload_request(&iq);
    assert!(matches!(result, Err(UploadError::BadRequest(_))));
}

#[test]
fn test_parse_upload_request_zero_size() {
    let request_elem = Element::builder("request", NS_HTTP_UPLOAD)
        .attr(
            minidom::rxml::xml_ncname!("filename").to_owned(),
            "test.txt",
        )
        .attr(minidom::rxml::xml_ncname!("size").to_owned(), "0")
        .build();
    let iq = Iq::Get {
        from: None,
        to: None,
        id: "upload-6".to_string(),
        payload: request_elem,
    };

    let result = parse_upload_request(&iq);
    assert!(matches!(result, Err(UploadError::BadRequest(_))));
}

#[test]
fn test_build_upload_slot_response() {
    let request_elem = Element::builder("request", NS_HTTP_UPLOAD)
        .attr(
            minidom::rxml::xml_ncname!("filename").to_owned(),
            "test.jpg",
        )
        .attr(minidom::rxml::xml_ncname!("size").to_owned(), "1000")
        .build();
    let original_iq = Iq::Get {
        from: Some("user@example.com".parse().unwrap()),
        to: Some("upload.example.com".parse().unwrap()),
        id: "slot-1".to_string(),
        payload: request_elem,
    };

    let slot = UploadSlot {
        put_url: "https://upload.example.com/slot/abc123".to_string(),
        put_headers: vec![
            ("Authorization".to_string(), "Bearer xyz".to_string()),
            ("Content-Type".to_string(), "image/jpeg".to_string()),
        ],
        get_url: "https://files.example.com/abc123/test.jpg".to_string(),
    };

    let response = build_upload_slot_response(&original_iq, &slot);

    assert_eq!(response.id(), "slot-1");
    assert!(matches!(
        response,
        xmpp_parsers::iq::Iq::Result {
            payload: Some(_),
            ..
        }
    ));

    if let xmpp_parsers::iq::Iq::Result {
        payload: Some(elem),
        ..
    } = &response
    {
        assert_eq!(elem.name(), "slot");
        assert_eq!(elem.ns(), NS_HTTP_UPLOAD);

        // Check PUT element
        let put_elem = elem.get_child("put", NS_HTTP_UPLOAD).unwrap();
        assert_eq!(
            put_elem.attr("url"),
            Some("https://upload.example.com/slot/abc123")
        );

        // Check headers
        let headers: Vec<_> = put_elem.children().collect();
        assert_eq!(headers.len(), 2);

        // Check GET element
        let get_elem = elem.get_child("get", NS_HTTP_UPLOAD).unwrap();
        assert_eq!(
            get_elem.attr("url"),
            Some("https://files.example.com/abc123/test.jpg")
        );
    } else {
        panic!("Expected Result with slot element");
    }
}

#[test]
fn test_build_upload_slot_response_no_headers() {
    let request_elem = Element::builder("request", NS_HTTP_UPLOAD)
        .attr(
            minidom::rxml::xml_ncname!("filename").to_owned(),
            "test.txt",
        )
        .attr(minidom::rxml::xml_ncname!("size").to_owned(), "100")
        .build();
    let original_iq = Iq::Get {
        from: None,
        to: None,
        id: "slot-2".to_string(),
        payload: request_elem,
    };

    let slot = UploadSlot {
        put_url: "https://upload.example.com/abc".to_string(),
        put_headers: vec![],
        get_url: "https://files.example.com/abc".to_string(),
    };

    let response = build_upload_slot_response(&original_iq, &slot);

    if let xmpp_parsers::iq::Iq::Result {
        payload: Some(elem),
        ..
    } = &response
    {
        let put_elem = elem.get_child("put", NS_HTTP_UPLOAD).unwrap();
        assert!(put_elem.children().next().is_none());
    } else {
        panic!("Expected Result with slot element");
    }
}

#[test]
fn test_build_upload_error_file_too_large() {
    let request = upload_request_iq("error-1", "large.jpg", 20_000_000);
    let error_response =
        build_upload_error(&request, &UploadError::FileTooLarge { max_size: 10485760 });

    let Iq::Error {
        id, error, payload, ..
    } = error_response
    else {
        panic!("Expected upload error IQ");
    };
    assert_eq!(id, "error-1");
    assert_eq!(error.type_, ErrorType::Modify);
    assert_eq!(error.defined_condition, DefinedCondition::NotAcceptable);
    let app_error = error.other.expect("file-too-large app error");
    assert_eq!(app_error.name(), "file-too-large");
    assert_eq!(app_error.ns(), NS_HTTP_UPLOAD);
    assert_eq!(
        app_error
            .get_child("max-file-size", NS_HTTP_UPLOAD)
            .expect("max-file-size")
            .text(),
        "10485760"
    );
    assert_eq!(
        payload.expect("original request").attr("filename"),
        Some("large.jpg")
    );
}

#[test]
fn test_build_upload_error_not_allowed() {
    let error_response = build_upload_error(
        &upload_request_iq("error-2", "blocked.jpg", 100),
        &UploadError::NotAllowed,
    );

    let Iq::Error { id, error, .. } = error_response else {
        panic!("Expected upload error IQ");
    };
    assert_eq!(id, "error-2");
    assert_eq!(error.type_, ErrorType::Auth);
    assert_eq!(error.defined_condition, DefinedCondition::Forbidden);
    assert!(error.other.is_none());
}

#[test]
fn test_build_upload_error_quota_reached() {
    let error_response = build_upload_error(
        &upload_request_iq("error-3", "quota.jpg", 100),
        &UploadError::QuotaReached {
            retry_at: fixed_retry_at(),
        },
    );

    let Iq::Error { id, error, .. } = error_response else {
        panic!("Expected upload error IQ");
    };
    assert_eq!(id, "error-3");
    assert_eq!(error.type_, ErrorType::Wait);
    assert_eq!(
        error.defined_condition,
        DefinedCondition::ResourceConstraint
    );
    let retry = error.other.expect("retry app error");
    assert_eq!(retry.name(), "retry");
    assert_eq!(retry.ns(), NS_HTTP_UPLOAD);
    assert_eq!(retry.attr("stamp"), Some("2026-05-31T12:34:56Z"));
}

#[test]
fn test_upload_error_display() {
    assert_eq!(
        UploadError::FileTooLarge { max_size: 1000 }.to_string(),
        "File too large. Maximum size is 1000 bytes."
    );
    assert_eq!(
        UploadError::NotAllowed.to_string(),
        "Not allowed to upload files"
    );
    assert_eq!(
        UploadError::QuotaReached {
            retry_at: fixed_retry_at(),
        }
        .to_string(),
        "Upload quota exceeded"
    );
    assert_eq!(
        UploadError::BadRequest(UploadBadRequest::MissingSize).to_string(),
        "Bad request: missing size attribute"
    );
    assert_eq!(
        UploadError::InternalError.to_string(),
        "Internal server error"
    );
}

#[test]
fn test_sanitize_filename() {
    // Normal filename
    assert_eq!(sanitize_filename("test.jpg"), "test.jpg");

    // With path components
    assert_eq!(sanitize_filename("/path/to/test.jpg"), "test.jpg");
    assert_eq!(sanitize_filename("C:\\Users\\test.jpg"), "test.jpg");

    // With special characters
    assert_eq!(sanitize_filename("my file (1).jpg"), "my_file__1_.jpg");
    assert_eq!(sanitize_filename("hello<world>.txt"), "hello_world_.txt");

    // Edge cases
    assert_eq!(sanitize_filename(""), "file");
    assert_eq!(sanitize_filename("."), "file");
    assert_eq!(sanitize_filename(".."), "file");

    // Valid characters preserved
    assert_eq!(
        sanitize_filename("test-file_v2.0.jpg"),
        "test-file_v2.0.jpg"
    );
}

#[test]
fn test_effective_content_type() {
    assert_eq!(effective_content_type(Some("image/jpeg")), "image/jpeg");
    assert_eq!(effective_content_type(None), "application/octet-stream");
}

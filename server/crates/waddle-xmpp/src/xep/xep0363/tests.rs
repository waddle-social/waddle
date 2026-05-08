use super::*;

#[test]
fn test_is_upload_request() {
    let request_elem = Element::builder("request", NS_HTTP_UPLOAD)
        .attr("filename", "test.jpg")
        .attr("size", "12345")
        .build();
    let iq = Iq {
        from: None,
        to: None,
        id: "upload-1".to_string(),
        payload: xmpp_parsers::iq::IqType::Get(request_elem),
    };

    assert!(is_upload_request(&iq));
}

#[test]
fn test_is_not_upload_request_wrong_ns() {
    let elem = Element::builder("request", "wrong:namespace")
        .attr("filename", "test.jpg")
        .attr("size", "12345")
        .build();
    let iq = Iq {
        from: None,
        to: None,
        id: "test-1".to_string(),
        payload: xmpp_parsers::iq::IqType::Get(elem),
    };

    assert!(!is_upload_request(&iq));
}

#[test]
fn test_is_not_upload_request_wrong_type() {
    let elem = Element::builder("request", NS_HTTP_UPLOAD)
        .attr("filename", "test.jpg")
        .attr("size", "12345")
        .build();
    let iq = Iq {
        from: None,
        to: None,
        id: "test-2".to_string(),
        payload: xmpp_parsers::iq::IqType::Set(elem),
    };

    assert!(!is_upload_request(&iq));
}

#[test]
fn test_parse_upload_request_full() {
    let request_elem = Element::builder("request", NS_HTTP_UPLOAD)
        .attr("filename", "vacation.jpg")
        .attr("size", "23456")
        .attr("content-type", "image/jpeg")
        .build();
    let iq = Iq {
        from: Some("user@example.com".parse().unwrap()),
        to: Some("upload.example.com".parse().unwrap()),
        id: "upload-1".to_string(),
        payload: xmpp_parsers::iq::IqType::Get(request_elem),
    };

    let request = parse_upload_request(&iq).unwrap();

    assert_eq!(request.filename, "vacation.jpg");
    assert_eq!(request.size, 23456);
    assert_eq!(request.content_type, Some("image/jpeg".to_string()));
}

#[test]
fn test_parse_upload_request_minimal() {
    let request_elem = Element::builder("request", NS_HTTP_UPLOAD)
        .attr("filename", "file.bin")
        .attr("size", "100")
        .build();
    let iq = Iq {
        from: None,
        to: None,
        id: "upload-2".to_string(),
        payload: xmpp_parsers::iq::IqType::Get(request_elem),
    };

    let request = parse_upload_request(&iq).unwrap();

    assert_eq!(request.filename, "file.bin");
    assert_eq!(request.size, 100);
    assert!(request.content_type.is_none());
}

#[test]
fn test_parse_upload_request_missing_filename() {
    let request_elem = Element::builder("request", NS_HTTP_UPLOAD)
        .attr("size", "100")
        .build();
    let iq = Iq {
        from: None,
        to: None,
        id: "upload-3".to_string(),
        payload: xmpp_parsers::iq::IqType::Get(request_elem),
    };

    let result = parse_upload_request(&iq);
    assert!(matches!(result, Err(UploadError::BadRequest(_))));
}

#[test]
fn test_parse_upload_request_missing_size() {
    let request_elem = Element::builder("request", NS_HTTP_UPLOAD)
        .attr("filename", "test.txt")
        .build();
    let iq = Iq {
        from: None,
        to: None,
        id: "upload-4".to_string(),
        payload: xmpp_parsers::iq::IqType::Get(request_elem),
    };

    let result = parse_upload_request(&iq);
    assert!(matches!(result, Err(UploadError::BadRequest(_))));
}

#[test]
fn test_parse_upload_request_invalid_size() {
    let request_elem = Element::builder("request", NS_HTTP_UPLOAD)
        .attr("filename", "test.txt")
        .attr("size", "not-a-number")
        .build();
    let iq = Iq {
        from: None,
        to: None,
        id: "upload-5".to_string(),
        payload: xmpp_parsers::iq::IqType::Get(request_elem),
    };

    let result = parse_upload_request(&iq);
    assert!(matches!(result, Err(UploadError::BadRequest(_))));
}

#[test]
fn test_parse_upload_request_zero_size() {
    let request_elem = Element::builder("request", NS_HTTP_UPLOAD)
        .attr("filename", "test.txt")
        .attr("size", "0")
        .build();
    let iq = Iq {
        from: None,
        to: None,
        id: "upload-6".to_string(),
        payload: xmpp_parsers::iq::IqType::Get(request_elem),
    };

    let result = parse_upload_request(&iq);
    assert!(matches!(result, Err(UploadError::BadRequest(_))));
}

#[test]
fn test_build_upload_slot_response() {
    let request_elem = Element::builder("request", NS_HTTP_UPLOAD)
        .attr("filename", "test.jpg")
        .attr("size", "1000")
        .build();
    let original_iq = Iq {
        from: Some("user@example.com".parse().unwrap()),
        to: Some("upload.example.com".parse().unwrap()),
        id: "slot-1".to_string(),
        payload: xmpp_parsers::iq::IqType::Get(request_elem),
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

    assert_eq!(response.id, "slot-1");
    assert!(matches!(
        response.payload,
        xmpp_parsers::iq::IqType::Result(Some(_))
    ));

    if let xmpp_parsers::iq::IqType::Result(Some(elem)) = &response.payload {
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
        .attr("filename", "test.txt")
        .attr("size", "100")
        .build();
    let original_iq = Iq {
        from: None,
        to: None,
        id: "slot-2".to_string(),
        payload: xmpp_parsers::iq::IqType::Get(request_elem),
    };

    let slot = UploadSlot {
        put_url: "https://upload.example.com/abc".to_string(),
        put_headers: vec![],
        get_url: "https://files.example.com/abc".to_string(),
    };

    let response = build_upload_slot_response(&original_iq, &slot);

    if let xmpp_parsers::iq::IqType::Result(Some(elem)) = &response.payload {
        let put_elem = elem.get_child("put", NS_HTTP_UPLOAD).unwrap();
        assert!(put_elem.children().next().is_none());
    } else {
        panic!("Expected Result with slot element");
    }
}

#[test]
fn test_build_upload_error_file_too_large() {
    let error_response =
        build_upload_error("error-1", &UploadError::FileTooLarge { max_size: 10485760 });

    assert!(error_response.contains("type='error'"));
    assert!(error_response.contains("id='error-1'"));
    assert!(error_response.contains("<not-acceptable"));
    assert!(error_response.contains("<file-too-large"));
    assert!(error_response.contains("<max-file-size>10485760</max-file-size>"));
}

#[test]
fn test_build_upload_error_not_allowed() {
    let error_response = build_upload_error("error-2", &UploadError::NotAllowed);

    assert!(error_response.contains("type='error'"));
    assert!(error_response.contains("id='error-2'"));
    assert!(error_response.contains("<forbidden"));
}

#[test]
fn test_build_upload_error_quota_reached() {
    let error_response = build_upload_error("error-3", &UploadError::QuotaReached);

    assert!(error_response.contains("type='error'"));
    assert!(error_response.contains("<resource-constraint"));
    assert!(error_response.contains("<retry"));
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
        UploadError::QuotaReached.to_string(),
        "Upload quota exceeded"
    );
    assert_eq!(
        UploadError::BadRequest("test".to_string()).to_string(),
        "Bad request: test"
    );
    assert_eq!(
        UploadError::InternalError("err".to_string()).to_string(),
        "Internal error: err"
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

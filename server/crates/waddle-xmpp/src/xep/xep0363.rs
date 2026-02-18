//! XEP-0363: HTTP File Upload
//!
//! Provides server-side support for HTTP-based file uploads in XMPP. Clients
//! request an upload slot from the server, then upload the file directly to
//! the HTTP endpoint. The server returns both upload (PUT) and download (GET)
//! URLs.
//!
//! ## Overview
//!
//! The upload flow is:
//! 1. Client sends slot request IQ with filename, size, and content-type
//! 2. Server validates request (size limits, permissions)
//! 3. Server returns PUT URL for upload and GET URL for retrieval
//! 4. Client uploads file via HTTP PUT
//! 5. Client shares GET URL in messages
//!
//! ## XML Format
//!
//! Request:
//! ```xml
//! <iq type='get' to='upload.example.com' id='upload-1'>
//!   <request xmlns='urn:xmpp:http:upload:0'
//!            filename='vacation.jpg'
//!            size='23456'
//!            content-type='image/jpeg'/>
//! </iq>
//! ```
//!
//! Response:
//! ```xml
//! <iq type='result' id='upload-1'>
//!   <slot xmlns='urn:xmpp:http:upload:0'>
//!     <put url='https://upload.example.com/slot/abc123'>
//!       <header name='Authorization'>Bearer xyz</header>
//!       <header name='Content-Type'>image/jpeg</header>
//!     </put>
//!     <get url='https://files.example.com/abc123/vacation.jpg'/>
//!   </slot>
//! </iq>
//! ```
//!
//! Error (file too large):
//! ```xml
//! <iq type='error' id='upload-1'>
//!   <request xmlns='urn:xmpp:http:upload:0'/>
//!   <error type='modify'>
//!     <not-acceptable xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/>
//!     <text xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'>
//!       File too large. Maximum size is 10485760 bytes.
//!     </text>
//!     <file-too-large xmlns='urn:xmpp:http:upload:0'>
//!       <max-file-size>10485760</max-file-size>
//!     </file-too-large>
//!   </error>
//! </iq>
//! ```

use minidom::Element;
use tracing::debug;
use xmpp_parsers::iq::Iq;

/// Namespace for XEP-0363 HTTP File Upload.
pub const NS_HTTP_UPLOAD: &str = "urn:xmpp:http:upload:0";

/// Default maximum file size (10 MB).
pub const DEFAULT_MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;

/// Parsed upload slot request.
#[derive(Debug, Clone)]
pub struct UploadRequest {
    /// Original filename.
    pub filename: String,
    /// File size in bytes.
    pub size: u64,
    /// MIME content type (optional, defaults to application/octet-stream).
    pub content_type: Option<String>,
}

/// Upload slot response containing PUT and GET URLs.
#[derive(Debug, Clone)]
pub struct UploadSlot {
    /// URL for uploading the file (HTTP PUT).
    pub put_url: String,
    /// Optional headers to include with the PUT request.
    pub put_headers: Vec<(String, String)>,
    /// URL for retrieving the file (HTTP GET).
    pub get_url: String,
}

/// Errors that can occur during HTTP file upload processing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadError {
    /// File exceeds the maximum allowed size.
    FileTooLarge { max_size: u64 },
    /// User is not allowed to upload files.
    NotAllowed,
    /// User has exceeded their upload quota.
    QuotaReached,
    /// Bad request (missing or invalid attributes).
    BadRequest(String),
    /// Internal server error.
    InternalError(String),
}

impl std::fmt::Display for UploadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UploadError::FileTooLarge { max_size } => {
                write!(f, "File too large. Maximum size is {} bytes.", max_size)
            }
            UploadError::NotAllowed => write!(f, "Not allowed to upload files"),
            UploadError::QuotaReached => write!(f, "Upload quota exceeded"),
            UploadError::BadRequest(msg) => write!(f, "Bad request: {}", msg),
            UploadError::InternalError(msg) => write!(f, "Internal error: {}", msg),
        }
    }
}

impl std::error::Error for UploadError {}

/// Check if an IQ stanza is an HTTP upload slot request (XEP-0363).
pub fn is_upload_request(iq: &Iq) -> bool {
    match &iq.payload {
        xmpp_parsers::iq::IqType::Get(elem) => {
            elem.name() == "request" && elem.ns() == NS_HTTP_UPLOAD
        }
        _ => false,
    }
}

/// Parse an upload slot request from an IQ stanza.
///
/// Returns the parsed request with filename, size, and optional content-type.
pub fn parse_upload_request(iq: &Iq) -> Result<UploadRequest, UploadError> {
    let elem = match &iq.payload {
        xmpp_parsers::iq::IqType::Get(elem) => {
            if elem.name() == "request" && elem.ns() == NS_HTTP_UPLOAD {
                elem
            } else {
                return Err(UploadError::BadRequest(
                    "Missing upload request element".to_string(),
                ));
            }
        }
        _ => {
            return Err(UploadError::BadRequest(
                "Expected IQ get for upload request".to_string(),
            ))
        }
    };

    // Parse required 'filename' attribute
    let filename = elem
        .attr("filename")
        .ok_or_else(|| UploadError::BadRequest("Missing 'filename' attribute".to_string()))?
        .to_string();

    if filename.is_empty() {
        return Err(UploadError::BadRequest(
            "Filename cannot be empty".to_string(),
        ));
    }

    // Parse required 'size' attribute
    let size_str = elem
        .attr("size")
        .ok_or_else(|| UploadError::BadRequest("Missing 'size' attribute".to_string()))?;

    let size: u64 = size_str
        .parse()
        .map_err(|_| UploadError::BadRequest(format!("Invalid 'size' attribute: {}", size_str)))?;

    if size == 0 {
        return Err(UploadError::BadRequest(
            "File size cannot be zero".to_string(),
        ));
    }

    // Parse optional 'content-type' attribute
    let content_type = elem.attr("content-type").map(|s| s.to_string());

    debug!(
        filename = %filename,
        size = size,
        content_type = ?content_type,
        "Parsed upload request"
    );

    Ok(UploadRequest {
        filename,
        size,
        content_type,
    })
}

/// Build an upload slot response IQ.
///
/// Returns an IQ result containing the PUT and GET URLs for the file.
pub fn build_upload_slot_response(original_iq: &Iq, slot: &UploadSlot) -> Iq {
    // Build PUT element with URL and optional headers
    let mut put_builder = Element::builder("put", NS_HTTP_UPLOAD).attr("url", &slot.put_url);

    for (name, value) in &slot.put_headers {
        let header_elem = Element::builder("header", NS_HTTP_UPLOAD)
            .attr("name", name)
            .append(value.as_str())
            .build();
        put_builder = put_builder.append(header_elem);
    }

    // Build GET element with URL
    let get_elem = Element::builder("get", NS_HTTP_UPLOAD)
        .attr("url", &slot.get_url)
        .build();

    // Build slot element containing PUT and GET
    let slot_elem = Element::builder("slot", NS_HTTP_UPLOAD)
        .append(put_builder.build())
        .append(get_elem)
        .build();

    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: xmpp_parsers::iq::IqType::Result(Some(slot_elem)),
    }
}

/// Build an upload error response IQ.
///
/// Returns an IQ error with the appropriate XMPP error condition and
/// XEP-0363 specific error elements.
pub fn build_upload_error(request_id: &str, error: &UploadError) -> String {
    let (error_type, condition, app_error) = match error {
        UploadError::FileTooLarge { max_size } => (
            "modify",
            "not-acceptable",
            format!(
                "<file-too-large xmlns='{}'><max-file-size>{}</max-file-size></file-too-large>",
                NS_HTTP_UPLOAD, max_size
            ),
        ),
        UploadError::NotAllowed => ("auth", "forbidden", String::new()),
        UploadError::QuotaReached => (
            "wait",
            "resource-constraint",
            format!("<retry xmlns='{}'/>", NS_HTTP_UPLOAD),
        ),
        UploadError::BadRequest(_) => ("modify", "bad-request", String::new()),
        UploadError::InternalError(_) => ("wait", "internal-server-error", String::new()),
    };

    let text = format!(
        "<text xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'>{}</text>",
        escape_xml(&error.to_string())
    );

    format!(
        "<iq type='error' id='{}'>\
            <request xmlns='{}'/>\
            <error type='{}'>\
                <{} xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/>\
                {}\
                {}\
            </error>\
        </iq>",
        escape_xml(request_id),
        NS_HTTP_UPLOAD,
        error_type,
        condition,
        text,
        app_error
    )
}

/// Escape XML special characters.
fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Sanitize filename for use in URLs and storage.
///
/// Removes path components, replaces unsafe characters, and limits length.
pub fn sanitize_filename(filename: &str) -> String {
    // Extract just the filename (remove any path components)
    let name = filename.rsplit(['/', '\\']).next().unwrap_or(filename);

    // Replace problematic characters with underscores
    let sanitized: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();

    // Limit length (max 255 chars)
    let truncated = if sanitized.len() > 255 {
        sanitized[..255].to_string()
    } else {
        sanitized
    };

    // Ensure we have a valid filename
    if truncated.is_empty() || truncated == "." || truncated == ".." {
        "file".to_string()
    } else {
        truncated
    }
}

/// Get the effective content type, with a sensible default.
pub fn effective_content_type(content_type: Option<&str>) -> &str {
    content_type.unwrap_or("application/octet-stream")
}

#[cfg(test)]
mod tests {
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
}

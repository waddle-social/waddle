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

use chrono::{DateTime, SecondsFormat, Utc};
use minidom::Element;
use tracing::debug;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

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
    QuotaReached { retry_at: DateTime<Utc> },
    /// Bad request (missing or invalid attributes).
    BadRequest(UploadBadRequest),
    /// Internal server error.
    InternalError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadBadRequest {
    MissingRequestElement,
    ExpectedIqGet,
    MissingFilename,
    EmptyFilename,
    MissingSize,
    InvalidSize,
    ZeroSize,
}

impl std::fmt::Display for UploadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UploadError::FileTooLarge { max_size } => {
                write!(f, "File too large. Maximum size is {} bytes.", max_size)
            }
            UploadError::NotAllowed => write!(f, "Not allowed to upload files"),
            UploadError::QuotaReached { .. } => write!(f, "Upload quota exceeded"),
            UploadError::BadRequest(error) => write!(f, "Bad request: {}", error),
            UploadError::InternalError => write!(f, "Internal server error"),
        }
    }
}

impl std::error::Error for UploadError {}

impl std::fmt::Display for UploadBadRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UploadBadRequest::MissingRequestElement => write!(f, "missing upload request element"),
            UploadBadRequest::ExpectedIqGet => write!(f, "expected IQ get for upload request"),
            UploadBadRequest::MissingFilename => write!(f, "missing filename attribute"),
            UploadBadRequest::EmptyFilename => write!(f, "filename cannot be empty"),
            UploadBadRequest::MissingSize => write!(f, "missing size attribute"),
            UploadBadRequest::InvalidSize => write!(f, "invalid size attribute"),
            UploadBadRequest::ZeroSize => write!(f, "file size cannot be zero"),
        }
    }
}

/// Check if an IQ stanza is an HTTP upload slot request (XEP-0363).
pub fn is_upload_request(iq: &Iq) -> bool {
    match iq {
        xmpp_parsers::iq::Iq::Get { payload: elem, .. } => {
            elem.name() == "request" && elem.ns() == NS_HTTP_UPLOAD
        }
        _ => false,
    }
}

/// Parse an upload slot request from an IQ stanza.
///
/// Returns the parsed request with filename, size, and optional content-type.
pub fn parse_upload_request(iq: &Iq) -> Result<UploadRequest, UploadError> {
    let elem = match iq {
        xmpp_parsers::iq::Iq::Get { payload: elem, .. } => {
            if elem.name() == "request" && elem.ns() == NS_HTTP_UPLOAD {
                elem
            } else {
                return Err(UploadError::BadRequest(
                    UploadBadRequest::MissingRequestElement,
                ));
            }
        }
        _ => return Err(UploadError::BadRequest(UploadBadRequest::ExpectedIqGet)),
    };

    // Parse required 'filename' attribute
    let filename = elem
        .attr("filename")
        .ok_or(UploadError::BadRequest(UploadBadRequest::MissingFilename))?
        .to_string();

    if filename.is_empty() {
        return Err(UploadError::BadRequest(UploadBadRequest::EmptyFilename));
    }

    // Parse required 'size' attribute
    let size_str = elem
        .attr("size")
        .ok_or(UploadError::BadRequest(UploadBadRequest::MissingSize))?;

    let size: u64 = size_str
        .parse()
        .map_err(|_| UploadError::BadRequest(UploadBadRequest::InvalidSize))?;

    if size == 0 {
        return Err(UploadError::BadRequest(UploadBadRequest::ZeroSize));
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
    let mut put_builder = Element::builder("put", NS_HTTP_UPLOAD)
        .attr(minidom::rxml::xml_ncname!("url").to_owned(), &slot.put_url);

    for (name, value) in &slot.put_headers {
        let header_elem = Element::builder("header", NS_HTTP_UPLOAD)
            .attr(minidom::rxml::xml_ncname!("name").to_owned(), name)
            .append(value.as_str())
            .build();
        put_builder = put_builder.append(header_elem);
    }

    // Build GET element with URL
    let get_elem = Element::builder("get", NS_HTTP_UPLOAD)
        .attr(minidom::rxml::xml_ncname!("url").to_owned(), &slot.get_url)
        .build();

    // Build slot element containing PUT and GET
    let slot_elem = Element::builder("slot", NS_HTTP_UPLOAD)
        .append(put_builder.build())
        .append(get_elem)
        .build();

    Iq::Result {
        from: original_iq.to().cloned(),
        to: original_iq.from().cloned(),
        id: original_iq.id().to_string(),
        payload: Some(slot_elem),
    }
}

/// Build an upload error response IQ.
///
/// Returns an IQ error with the appropriate XMPP error condition
/// and XEP-0363-specific app-error elements per §"Error
/// conditions": `<file-too-large><max-file-size>` for
/// FileTooLarge, `<retry stamp='...'/>` for QuotaReached.
pub fn build_upload_error(original_iq: &Iq, error: &UploadError) -> Iq {
    let (error_type, defined_condition) = match error {
        UploadError::FileTooLarge { .. } => (ErrorType::Modify, DefinedCondition::NotAcceptable),
        UploadError::NotAllowed => (ErrorType::Auth, DefinedCondition::Forbidden),
        UploadError::QuotaReached { .. } => (ErrorType::Wait, DefinedCondition::ResourceConstraint),
        UploadError::BadRequest(_) => (ErrorType::Modify, DefinedCondition::BadRequest),
        UploadError::InternalError => (ErrorType::Wait, DefinedCondition::InternalServerError),
    };

    let mut stanza_error = StanzaError::new(error_type, defined_condition, "en", error.to_string());

    stanza_error.other = match error {
        UploadError::FileTooLarge { max_size } => Some(
            Element::builder("file-too-large", NS_HTTP_UPLOAD)
                .append(
                    Element::builder("max-file-size", NS_HTTP_UPLOAD)
                        .append(max_size.to_string().as_str())
                        .build(),
                )
                .build(),
        ),
        UploadError::QuotaReached { retry_at } => {
            let stamp = retry_at.to_rfc3339_opts(SecondsFormat::Secs, true);
            Some(
                Element::builder("retry", NS_HTTP_UPLOAD)
                    .attr(minidom::rxml::xml_ncname!("stamp").to_owned(), stamp)
                    .build(),
            )
        }
        _ => None,
    };

    Iq::Error {
        from: original_iq.to().cloned(),
        to: original_iq.from().cloned(),
        id: original_iq.id().to_string(),
        error: stanza_error,
        payload: upload_error_payload(original_iq),
    }
}

fn upload_error_payload(original_iq: &Iq) -> Option<Element> {
    match original_iq {
        Iq::Get { payload, .. } | Iq::Set { payload, .. } => Some(payload.clone()),
        Iq::Result { .. } | Iq::Error { .. } => None,
    }
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
mod tests;

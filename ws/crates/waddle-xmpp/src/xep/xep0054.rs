//! XEP-0054: vcard-temp
//!
//! Provides user profile information via vCard format. This is a legacy protocol
//! still widely used for basic profile information like display name, photo,
//! email, and other contact details.
//!
//! ## Overview
//!
//! vcard-temp allows users to:
//! - Retrieve their own vCard (IQ get to self)
//! - Retrieve another user's vCard (IQ get with 'to' attribute)
//! - Set/update their own vCard (IQ set)
//!
//! ## XML Format
//!
//! ```xml
//! <vCard xmlns='vcard-temp'>
//!   <FN>Full Name</FN>
//!   <NICKNAME>Nick</NICKNAME>
//!   <PHOTO>
//!     <TYPE>image/png</TYPE>
//!     <BINVAL>base64-encoded-data</BINVAL>
//!   </PHOTO>
//!   <EMAIL><INTERNET/><PREF/><USERID>user@example.com</USERID></EMAIL>
//!   <NOTE>About me text</NOTE>
//!   <URL>https://example.com</URL>
//! </vCard>
//! ```

use minidom::Element;
use tracing::debug;
use xmpp_parsers::iq::Iq;

/// Namespace for XEP-0054 vcard-temp.
pub const NS_VCARD: &str = "vcard-temp";

/// vCard data structure representing user profile information.
#[derive(Debug, Clone, Default)]
pub struct VCard {
    /// Full name (FN element)
    pub full_name: Option<String>,
    /// Nickname (NICKNAME element)
    pub nickname: Option<String>,
    /// Photo data (PHOTO element)
    pub photo: Option<VCardPhoto>,
    /// Email address (EMAIL element)
    pub email: Option<String>,
    /// Note/description (NOTE element)
    pub note: Option<String>,
    /// URL/website (URL element)
    pub url: Option<String>,
    /// Birthday (BDAY element, ISO 8601 format)
    pub birthday: Option<String>,
    /// Organization name (ORG element)
    pub org: Option<String>,
    /// Title/role (TITLE element)
    pub title: Option<String>,
}

/// Photo data for vCard.
#[derive(Debug, Clone)]
pub struct VCardPhoto {
    /// MIME type (e.g., "image/png", "image/jpeg")
    pub mime_type: String,
    /// Base64-encoded photo data
    pub data: String,
}

/// Errors that can occur during vCard processing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VCardError {
    /// vCard not found for the requested user
    NotFound,
    /// Bad request (malformed vCard)
    BadRequest(String),
    /// Internal server error
    InternalError(String),
    /// Not authorized to access this vCard
    NotAuthorized,
}

impl std::fmt::Display for VCardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VCardError::NotFound => write!(f, "vCard not found"),
            VCardError::BadRequest(msg) => write!(f, "Bad request: {}", msg),
            VCardError::InternalError(msg) => write!(f, "Internal error: {}", msg),
            VCardError::NotAuthorized => write!(f, "Not authorized"),
        }
    }
}

impl std::error::Error for VCardError {}

/// Check if an IQ stanza is a vCard query (XEP-0054).
///
/// Returns true for both `get` (retrieve vCard) and `set` (update vCard) types.
pub fn is_vcard_query(iq: &Iq) -> bool {
    match &iq.payload {
        xmpp_parsers::iq::IqType::Get(elem) | xmpp_parsers::iq::IqType::Set(elem) => {
            elem.name() == "vCard" && elem.ns() == NS_VCARD
        }
        _ => false,
    }
}

/// Check if an IQ is a vCard get request.
pub fn is_vcard_get(iq: &Iq) -> bool {
    match &iq.payload {
        xmpp_parsers::iq::IqType::Get(elem) => elem.name() == "vCard" && elem.ns() == NS_VCARD,
        _ => false,
    }
}

/// Check if an IQ is a vCard set request.
pub fn is_vcard_set(iq: &Iq) -> bool {
    match &iq.payload {
        xmpp_parsers::iq::IqType::Set(elem) => elem.name() == "vCard" && elem.ns() == NS_VCARD,
        _ => false,
    }
}

/// Parse a vCard from an IQ set stanza.
///
/// Returns the parsed VCard data for storage.
pub fn parse_vcard_from_iq(iq: &Iq) -> Result<VCard, VCardError> {
    let elem = match &iq.payload {
        xmpp_parsers::iq::IqType::Set(elem) => {
            if elem.name() == "vCard" && elem.ns() == NS_VCARD {
                elem
            } else {
                return Err(VCardError::BadRequest("Missing vCard element".to_string()));
            }
        }
        _ => {
            return Err(VCardError::BadRequest(
                "Expected IQ set for vCard update".to_string(),
            ))
        }
    };

    parse_vcard_element(elem)
}

/// Parse vCard data from an Element.
pub fn parse_vcard_element(elem: &Element) -> Result<VCard, VCardError> {
    let mut vcard = VCard::default();

    // Parse FN (Full Name)
    if let Some(fn_elem) = elem.get_child("FN", NS_VCARD) {
        let text = fn_elem.text();
        if !text.is_empty() {
            vcard.full_name = Some(text);
        }
    }

    // Parse NICKNAME
    if let Some(nick_elem) = elem.get_child("NICKNAME", NS_VCARD) {
        let text = nick_elem.text();
        if !text.is_empty() {
            vcard.nickname = Some(text);
        }
    }

    // Parse PHOTO
    if let Some(photo_elem) = elem.get_child("PHOTO", NS_VCARD) {
        let mime_type = photo_elem
            .get_child("TYPE", NS_VCARD)
            .map(|e| e.text())
            .filter(|s| !s.is_empty());
        let data = photo_elem
            .get_child("BINVAL", NS_VCARD)
            .map(|e| e.text())
            .filter(|s| !s.is_empty());

        if let (Some(mime_type), Some(data)) = (mime_type, data) {
            vcard.photo = Some(VCardPhoto { mime_type, data });
        }
    }

    // Parse EMAIL - supports simple or structured format
    if let Some(email_elem) = elem.get_child("EMAIL", NS_VCARD) {
        // Try structured format first (with USERID child)
        if let Some(userid_elem) = email_elem.get_child("USERID", NS_VCARD) {
            let text = userid_elem.text();
            if !text.is_empty() {
                vcard.email = Some(text);
            }
        } else {
            // Fall back to simple text content
            let text = email_elem.text();
            if !text.is_empty() {
                vcard.email = Some(text);
            }
        }
    }

    // Parse NOTE
    if let Some(note_elem) = elem.get_child("NOTE", NS_VCARD) {
        let text = note_elem.text();
        if !text.is_empty() {
            vcard.note = Some(text);
        }
    }

    // Parse URL
    if let Some(url_elem) = elem.get_child("URL", NS_VCARD) {
        let text = url_elem.text();
        if !text.is_empty() {
            vcard.url = Some(text);
        }
    }

    // Parse BDAY (Birthday)
    if let Some(bday_elem) = elem.get_child("BDAY", NS_VCARD) {
        let text = bday_elem.text();
        if !text.is_empty() {
            vcard.birthday = Some(text);
        }
    }

    // Parse ORG (Organization)
    if let Some(org_elem) = elem.get_child("ORG", NS_VCARD) {
        // Try ORGNAME child first
        if let Some(orgname_elem) = org_elem.get_child("ORGNAME", NS_VCARD) {
            let text = orgname_elem.text();
            if !text.is_empty() {
                vcard.org = Some(text);
            }
        } else {
            // Fall back to text content
            let text = org_elem.text();
            if !text.is_empty() {
                vcard.org = Some(text);
            }
        }
    }

    // Parse TITLE
    if let Some(title_elem) = elem.get_child("TITLE", NS_VCARD) {
        let text = title_elem.text();
        if !text.is_empty() {
            vcard.title = Some(text);
        }
    }

    debug!(
        full_name = ?vcard.full_name,
        nickname = ?vcard.nickname,
        has_photo = vcard.photo.is_some(),
        "Parsed vCard"
    );

    Ok(vcard)
}

/// Build a vCard element from VCard data.
pub fn build_vcard_element(vcard: &VCard) -> Element {
    let mut builder = Element::builder("vCard", NS_VCARD);

    // Add FN
    if let Some(ref full_name) = vcard.full_name {
        builder = builder.append(
            Element::builder("FN", NS_VCARD)
                .append(full_name.as_str())
                .build(),
        );
    }

    // Add NICKNAME
    if let Some(ref nickname) = vcard.nickname {
        builder = builder.append(
            Element::builder("NICKNAME", NS_VCARD)
                .append(nickname.as_str())
                .build(),
        );
    }

    // Add PHOTO
    if let Some(ref photo) = vcard.photo {
        let photo_elem = Element::builder("PHOTO", NS_VCARD)
            .append(
                Element::builder("TYPE", NS_VCARD)
                    .append(photo.mime_type.as_str())
                    .build(),
            )
            .append(
                Element::builder("BINVAL", NS_VCARD)
                    .append(photo.data.as_str())
                    .build(),
            )
            .build();
        builder = builder.append(photo_elem);
    }

    // Add EMAIL (structured format)
    if let Some(ref email) = vcard.email {
        let email_elem = Element::builder("EMAIL", NS_VCARD)
            .append(Element::builder("INTERNET", NS_VCARD).build())
            .append(Element::builder("PREF", NS_VCARD).build())
            .append(
                Element::builder("USERID", NS_VCARD)
                    .append(email.as_str())
                    .build(),
            )
            .build();
        builder = builder.append(email_elem);
    }

    // Add NOTE
    if let Some(ref note) = vcard.note {
        builder = builder.append(
            Element::builder("NOTE", NS_VCARD)
                .append(note.as_str())
                .build(),
        );
    }

    // Add URL
    if let Some(ref url) = vcard.url {
        builder = builder.append(
            Element::builder("URL", NS_VCARD)
                .append(url.as_str())
                .build(),
        );
    }

    // Add BDAY
    if let Some(ref birthday) = vcard.birthday {
        builder = builder.append(
            Element::builder("BDAY", NS_VCARD)
                .append(birthday.as_str())
                .build(),
        );
    }

    // Add ORG
    if let Some(ref org) = vcard.org {
        let org_elem = Element::builder("ORG", NS_VCARD)
            .append(
                Element::builder("ORGNAME", NS_VCARD)
                    .append(org.as_str())
                    .build(),
            )
            .build();
        builder = builder.append(org_elem);
    }

    // Add TITLE
    if let Some(ref title) = vcard.title {
        builder = builder.append(
            Element::builder("TITLE", NS_VCARD)
                .append(title.as_str())
                .build(),
        );
    }

    builder.build()
}

/// Build a vCard IQ result response.
pub fn build_vcard_response(original_iq: &Iq, vcard: &VCard) -> Iq {
    let vcard_elem = build_vcard_element(vcard);

    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: xmpp_parsers::iq::IqType::Result(Some(vcard_elem)),
    }
}

/// Build an empty vCard IQ result response (no vCard found).
pub fn build_empty_vcard_response(original_iq: &Iq) -> Iq {
    // Return an empty vCard element for not-found case
    let vcard_elem = Element::builder("vCard", NS_VCARD).build();

    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: xmpp_parsers::iq::IqType::Result(Some(vcard_elem)),
    }
}

/// Build a vCard set success response (empty result).
pub fn build_vcard_success(original_iq: &Iq) -> Iq {
    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: xmpp_parsers::iq::IqType::Result(None),
    }
}

/// Build a vCard error response.
pub fn build_vcard_error(request_id: &str, error: &VCardError) -> String {
    let (error_type, condition) = match error {
        VCardError::NotFound => ("cancel", "item-not-found"),
        VCardError::BadRequest(_) => ("modify", "bad-request"),
        VCardError::InternalError(_) => ("wait", "internal-server-error"),
        VCardError::NotAuthorized => ("auth", "not-authorized"),
    };

    let text = match error {
        VCardError::BadRequest(msg) | VCardError::InternalError(msg) => {
            format!(
                "<text xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'>{}</text>",
                escape_xml(msg)
            )
        }
        _ => String::new(),
    };

    format!(
        "<iq type='error' id='{}'>\
            <vCard xmlns='{}'/>\
            <error type='{}'>\
                <{} xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/>\
                {}\
            </error>\
        </iq>",
        escape_xml(request_id),
        NS_VCARD,
        error_type,
        condition,
        text
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_vcard_query_get() {
        let vcard_elem = Element::builder("vCard", NS_VCARD).build();
        let iq = Iq {
            from: None,
            to: None,
            id: "vcard-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Get(vcard_elem),
        };

        assert!(is_vcard_query(&iq));
        assert!(is_vcard_get(&iq));
        assert!(!is_vcard_set(&iq));
    }

    #[test]
    fn test_is_vcard_query_set() {
        let vcard_elem = Element::builder("vCard", NS_VCARD)
            .append(Element::builder("FN", NS_VCARD).append("John Doe").build())
            .build();
        let iq = Iq {
            from: None,
            to: None,
            id: "vcard-2".to_string(),
            payload: xmpp_parsers::iq::IqType::Set(vcard_elem),
        };

        assert!(is_vcard_query(&iq));
        assert!(!is_vcard_get(&iq));
        assert!(is_vcard_set(&iq));
    }

    #[test]
    fn test_is_not_vcard_query_wrong_ns() {
        let elem = Element::builder("vCard", "wrong:namespace").build();
        let iq = Iq {
            from: None,
            to: None,
            id: "test-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Get(elem),
        };

        assert!(!is_vcard_query(&iq));
    }

    #[test]
    fn test_is_not_vcard_query_wrong_name() {
        let elem = Element::builder("query", NS_VCARD).build();
        let iq = Iq {
            from: None,
            to: None,
            id: "test-2".to_string(),
            payload: xmpp_parsers::iq::IqType::Get(elem),
        };

        assert!(!is_vcard_query(&iq));
    }

    #[test]
    fn test_parse_vcard_full() {
        let vcard_elem = Element::builder("vCard", NS_VCARD)
            .append(Element::builder("FN", NS_VCARD).append("John Doe").build())
            .append(
                Element::builder("NICKNAME", NS_VCARD)
                    .append("johnd")
                    .build(),
            )
            .append(
                Element::builder("EMAIL", NS_VCARD)
                    .append(Element::builder("INTERNET", NS_VCARD).build())
                    .append(
                        Element::builder("USERID", NS_VCARD)
                            .append("john@example.com")
                            .build(),
                    )
                    .build(),
            )
            .append(
                Element::builder("NOTE", NS_VCARD)
                    .append("Hello, world!")
                    .build(),
            )
            .append(
                Element::builder("URL", NS_VCARD)
                    .append("https://example.com")
                    .build(),
            )
            .build();

        let vcard = parse_vcard_element(&vcard_elem).unwrap();

        assert_eq!(vcard.full_name, Some("John Doe".to_string()));
        assert_eq!(vcard.nickname, Some("johnd".to_string()));
        assert_eq!(vcard.email, Some("john@example.com".to_string()));
        assert_eq!(vcard.note, Some("Hello, world!".to_string()));
        assert_eq!(vcard.url, Some("https://example.com".to_string()));
    }

    #[test]
    fn test_parse_vcard_with_photo() {
        let vcard_elem = Element::builder("vCard", NS_VCARD)
            .append(
                Element::builder("PHOTO", NS_VCARD)
                    .append(
                        Element::builder("TYPE", NS_VCARD)
                            .append("image/png")
                            .build(),
                    )
                    .append(
                        Element::builder("BINVAL", NS_VCARD)
                            .append("iVBORw0KGgo=")
                            .build(),
                    )
                    .build(),
            )
            .build();

        let vcard = parse_vcard_element(&vcard_elem).unwrap();

        assert!(vcard.photo.is_some());
        let photo = vcard.photo.unwrap();
        assert_eq!(photo.mime_type, "image/png");
        assert_eq!(photo.data, "iVBORw0KGgo=");
    }

    #[test]
    fn test_parse_vcard_empty() {
        let vcard_elem = Element::builder("vCard", NS_VCARD).build();

        let vcard = parse_vcard_element(&vcard_elem).unwrap();

        assert!(vcard.full_name.is_none());
        assert!(vcard.nickname.is_none());
        assert!(vcard.email.is_none());
        assert!(vcard.photo.is_none());
    }

    #[test]
    fn test_build_vcard_element() {
        let vcard = VCard {
            full_name: Some("Jane Doe".to_string()),
            nickname: Some("janed".to_string()),
            email: Some("jane@example.com".to_string()),
            note: Some("Test note".to_string()),
            url: Some("https://jane.example.com".to_string()),
            photo: None,
            birthday: Some("1990-01-15".to_string()),
            org: Some("Example Corp".to_string()),
            title: Some("Engineer".to_string()),
        };

        let elem = build_vcard_element(&vcard);

        assert_eq!(elem.name(), "vCard");
        assert_eq!(elem.ns(), NS_VCARD);

        // Check FN
        let fn_elem = elem.get_child("FN", NS_VCARD).unwrap();
        assert_eq!(fn_elem.text(), "Jane Doe");

        // Check NICKNAME
        let nick_elem = elem.get_child("NICKNAME", NS_VCARD).unwrap();
        assert_eq!(nick_elem.text(), "janed");

        // Check EMAIL structure
        let email_elem = elem.get_child("EMAIL", NS_VCARD).unwrap();
        assert!(email_elem.get_child("INTERNET", NS_VCARD).is_some());
        assert!(email_elem.get_child("PREF", NS_VCARD).is_some());
        let userid_elem = email_elem.get_child("USERID", NS_VCARD).unwrap();
        assert_eq!(userid_elem.text(), "jane@example.com");
    }

    #[test]
    fn test_build_vcard_response() {
        let vcard_elem = Element::builder("vCard", NS_VCARD).build();
        let original_iq = Iq {
            from: Some("user@example.com".parse().unwrap()),
            to: Some("server.example.com".parse().unwrap()),
            id: "vcard-get-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Get(vcard_elem),
        };

        let vcard = VCard {
            full_name: Some("Test User".to_string()),
            ..Default::default()
        };

        let response = build_vcard_response(&original_iq, &vcard);

        assert_eq!(response.id, "vcard-get-1");
        assert!(matches!(
            response.payload,
            xmpp_parsers::iq::IqType::Result(Some(_))
        ));
    }

    #[test]
    fn test_build_empty_vcard_response() {
        let vcard_elem = Element::builder("vCard", NS_VCARD).build();
        let original_iq = Iq {
            from: Some("user@example.com".parse().unwrap()),
            to: None,
            id: "vcard-get-2".to_string(),
            payload: xmpp_parsers::iq::IqType::Get(vcard_elem),
        };

        let response = build_empty_vcard_response(&original_iq);

        assert_eq!(response.id, "vcard-get-2");
        if let xmpp_parsers::iq::IqType::Result(Some(elem)) = &response.payload {
            assert_eq!(elem.name(), "vCard");
            assert_eq!(elem.ns(), NS_VCARD);
            // Empty vCard should have no children
            assert!(elem.children().next().is_none());
        } else {
            panic!("Expected Result with vCard element");
        }
    }

    #[test]
    fn test_build_vcard_success() {
        let vcard_elem = Element::builder("vCard", NS_VCARD).build();
        let original_iq = Iq {
            from: Some("user@example.com".parse().unwrap()),
            to: None,
            id: "vcard-set-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Set(vcard_elem),
        };

        let response = build_vcard_success(&original_iq);

        assert_eq!(response.id, "vcard-set-1");
        assert!(matches!(
            response.payload,
            xmpp_parsers::iq::IqType::Result(None)
        ));
    }

    #[test]
    fn test_build_vcard_error() {
        let error_response = build_vcard_error(
            "error-1",
            &VCardError::BadRequest("Invalid data".to_string()),
        );

        assert!(error_response.contains("type='error'"));
        assert!(error_response.contains("id='error-1'"));
        assert!(error_response.contains("<bad-request"));
        assert!(error_response.contains("Invalid data"));
    }

    #[test]
    fn test_vcard_error_display() {
        assert_eq!(VCardError::NotFound.to_string(), "vCard not found");
        assert_eq!(
            VCardError::BadRequest("test".to_string()).to_string(),
            "Bad request: test"
        );
        assert_eq!(
            VCardError::InternalError("err".to_string()).to_string(),
            "Internal error: err"
        );
        assert_eq!(VCardError::NotAuthorized.to_string(), "Not authorized");
    }

    #[test]
    fn test_roundtrip_vcard() {
        let original = VCard {
            full_name: Some("Round Trip".to_string()),
            nickname: Some("rt".to_string()),
            email: Some("rt@example.com".to_string()),
            note: Some("Testing roundtrip".to_string()),
            url: Some("https://roundtrip.example.com".to_string()),
            birthday: Some("2000-12-31".to_string()),
            org: Some("Test Org".to_string()),
            title: Some("Tester".to_string()),
            photo: Some(VCardPhoto {
                mime_type: "image/jpeg".to_string(),
                data: "dGVzdA==".to_string(),
            }),
        };

        let elem = build_vcard_element(&original);
        let parsed = parse_vcard_element(&elem).unwrap();

        assert_eq!(original.full_name, parsed.full_name);
        assert_eq!(original.nickname, parsed.nickname);
        assert_eq!(original.email, parsed.email);
        assert_eq!(original.note, parsed.note);
        assert_eq!(original.url, parsed.url);
        assert_eq!(original.birthday, parsed.birthday);
        assert_eq!(original.org, parsed.org);
        assert_eq!(original.title, parsed.title);
        assert!(parsed.photo.is_some());
        let photo = parsed.photo.unwrap();
        assert_eq!(photo.mime_type, "image/jpeg");
        assert_eq!(photo.data, "dGVzdA==");
    }
}

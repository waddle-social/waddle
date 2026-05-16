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
    /// Description (DESC element)
    pub desc: Option<String>,
}

/// Photo data for vCard — either inline base64 (BINVAL) or external URL (EXTVAL).
#[derive(Debug, Clone)]
pub enum VCardPhoto {
    /// Inline base64-encoded photo (TYPE + BINVAL).
    Binary {
        /// MIME type (e.g., "image/png", "image/jpeg")
        mime_type: String,
        /// Base64-encoded photo data
        data: String,
    },
    /// External photo URL (EXTVAL).
    External {
        /// URL pointing to the photo
        url: String,
    },
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

    // Parse PHOTO — supports both BINVAL (inline base64) and EXTVAL (URL)
    if let Some(photo_elem) = elem.get_child("PHOTO", NS_VCARD) {
        // Try EXTVAL first (URL reference)
        let extval = photo_elem
            .get_child("EXTVAL", NS_VCARD)
            .map(|e| e.text())
            .filter(|s| !s.is_empty());

        if let Some(url) = extval {
            vcard.photo = Some(VCardPhoto::External { url });
        } else {
            // Fall back to TYPE + BINVAL (inline base64)
            let mime_type = photo_elem
                .get_child("TYPE", NS_VCARD)
                .map(|e| e.text())
                .filter(|s| !s.is_empty());
            let data = photo_elem
                .get_child("BINVAL", NS_VCARD)
                .map(|e| e.text())
                .filter(|s| !s.is_empty());

            if let (Some(mime_type), Some(data)) = (mime_type, data) {
                vcard.photo = Some(VCardPhoto::Binary { mime_type, data });
            }
        }
    }

    // Parse DESC (Description)
    if let Some(desc_elem) = elem.get_child("DESC", NS_VCARD) {
        let text = desc_elem.text();
        if !text.is_empty() {
            vcard.desc = Some(text);
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
        let photo_elem = match photo {
            VCardPhoto::Binary { mime_type, data } => Element::builder("PHOTO", NS_VCARD)
                .append(
                    Element::builder("TYPE", NS_VCARD)
                        .append(mime_type.as_str())
                        .build(),
                )
                .append(
                    Element::builder("BINVAL", NS_VCARD)
                        .append(data.as_str())
                        .build(),
                )
                .build(),
            VCardPhoto::External { url } => Element::builder("PHOTO", NS_VCARD)
                .append(
                    Element::builder("EXTVAL", NS_VCARD)
                        .append(url.as_str())
                        .build(),
                )
                .build(),
        };
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

    // Add DESC (Description)
    if let Some(ref desc) = vcard.desc {
        builder = builder.append(
            Element::builder("DESC", NS_VCARD)
                .append(desc.as_str())
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

#[cfg(test)]
mod tests;

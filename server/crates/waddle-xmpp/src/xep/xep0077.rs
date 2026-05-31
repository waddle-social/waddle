//! XEP-0077: In-Band Registration
//!
//! Allows users to register accounts directly through the XMPP connection,
//! before authentication. Supports get (form request) and set (submit) IQs.

use minidom::Element;
use tracing::debug;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::ErrorType;

/// Namespace for XEP-0077 In-Band Registration IQ queries.
pub const NS_REGISTER: &str = "jabber:iq:register";

/// Namespace for XEP-0077 stream feature advertisement (per Section 8).
pub const NS_REGISTER_FEATURE: &str = "http://jabber.org/features/iq-register";

const NS_CLIENT: &str = "jabber:client";
const NS_XMPP_STANZAS: &str = "urn:ietf:params:xml:ns:xmpp-stanzas";

/// Registration request parsed from an IQ stanza.
#[derive(Debug, Clone)]
pub struct RegistrationRequest {
    /// The requested username (required)
    pub username: String,
    /// The password (required)
    pub password: String,
    /// Optional email address
    pub email: Option<String>,
}

/// Registration errors that can occur during XEP-0077 processing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistrationError {
    /// Registration is not allowed (disabled by server)
    NotAllowed,
    /// User already exists
    Conflict,
    /// Missing required field
    NotAcceptable(String),
    /// Invalid field value
    BadRequest(String),
    /// Internal server error
    InternalError(String),
}

/// Typed XEP-0077 registration error response.
#[derive(Debug, Clone, PartialEq)]
pub struct RegistrationErrorResponse {
    request_id: String,
    error_type: ErrorType,
    condition: RegistrationErrorCondition,
    legacy_code: LegacyRegistrationErrorCode,
    submitted_query: Element,
    text: Option<String>,
}

impl RegistrationErrorResponse {
    /// Convert to XML for the transport boundary.
    pub fn to_element(&self) -> Element {
        let mut error_element = Element::builder("error", NS_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("code").to_owned(),
                self.legacy_code.as_str(),
            )
            .attr(
                minidom::rxml::xml_ncname!("type").to_owned(),
                error_type_attr(&self.error_type),
            )
            .append(Element::builder(self.condition.element_name(), NS_XMPP_STANZAS).build());

        if let Some(text) = &self.text {
            error_element = error_element.append(
                Element::builder("text", NS_XMPP_STANZAS)
                    .append(text.as_str())
                    .build(),
            );
        }

        Element::builder("iq", NS_CLIENT)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "error")
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                self.request_id.as_str(),
            )
            .append(self.submitted_query.clone())
            .append(error_element.build())
            .build()
    }
}

impl std::fmt::Display for RegistrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegistrationError::NotAllowed => write!(f, "Registration is not allowed"),
            RegistrationError::Conflict => write!(f, "User already exists"),
            RegistrationError::NotAcceptable(msg) => write!(f, "Not acceptable: {}", msg),
            RegistrationError::BadRequest(msg) => write!(f, "Bad request: {}", msg),
            RegistrationError::InternalError(msg) => write!(f, "Internal error: {}", msg),
        }
    }
}

impl std::error::Error for RegistrationError {}

/// Check if an IQ stanza is a registration query (XEP-0077).
///
/// Returns true for both `get` (request form) and `set` (submit registration) types.
pub fn is_registration_query(iq: &Iq) -> bool {
    // Convert to Element and check for query child with register namespace
    let element: Element = iq.clone().into();
    element.get_child("query", NS_REGISTER).is_some()
}

/// Check if an IQ element is a registration query (for pre-auth parsing).
pub fn is_registration_query_element(element: &Element) -> bool {
    if element.name() != "iq" {
        return false;
    }
    element.get_child("query", NS_REGISTER).is_some()
}

/// Parse a registration IQ stanza.
///
/// Returns:
/// - `Ok(None)` for a `get` request (client wants the registration form)
/// - `Ok(Some(RegistrationRequest))` for a `set` request with valid fields
/// - `Err(RegistrationError)` for invalid requests
pub fn parse_registration_iq(iq: &Iq) -> Result<Option<RegistrationRequest>, RegistrationError> {
    let element: Element = iq.clone().into();
    parse_registration_element(&element, iq.id())
}

/// Parse a registration element (for pre-auth parsing where we have raw Element).
pub fn parse_registration_element(
    element: &Element,
    id: &str,
) -> Result<Option<RegistrationRequest>, RegistrationError> {
    let iq_type = element.attr("type").unwrap_or("");

    let query = element
        .get_child("query", NS_REGISTER)
        .ok_or_else(|| RegistrationError::BadRequest("Missing query element".to_string()))?;

    match iq_type {
        "get" => {
            // Client is requesting the registration form
            debug!(id = %id, "Registration form requested");
            Ok(None)
        }
        "set" => {
            // Client is submitting registration
            let username = query
                .get_child("username", NS_REGISTER)
                .map(|e| e.text())
                .unwrap_or_default();

            let password = query
                .get_child("password", NS_REGISTER)
                .map(|e| e.text())
                .unwrap_or_default();

            let email = query
                .get_child("email", NS_REGISTER)
                .map(|e| e.text())
                .filter(|s| !s.is_empty());

            // Validate required fields
            if username.is_empty() {
                return Err(RegistrationError::NotAcceptable(
                    "Username is required".to_string(),
                ));
            }

            if password.is_empty() {
                return Err(RegistrationError::NotAcceptable(
                    "Password is required".to_string(),
                ));
            }

            debug!(id = %id, username = %username, "Registration submission received");

            Ok(Some(RegistrationRequest {
                username,
                password,
                email,
            }))
        }
        _ => Err(RegistrationError::BadRequest(format!(
            "Invalid IQ type for registration: {}",
            iq_type
        ))),
    }
}

/// Build a registration fields response (reply to get request).
///
/// This tells the client what fields are required/optional for registration.
pub fn build_registration_fields_response(
    request_id: &str,
    instructions: Option<&str>,
    include_email: bool,
) -> Iq {
    let mut query = Element::builder("query", NS_REGISTER);

    if let Some(instructions) = instructions {
        query = query.append(
            Element::builder("instructions", NS_REGISTER)
                .append(instructions)
                .build(),
        );
    }

    query = query
        .append(Element::builder("username", NS_REGISTER).build())
        .append(Element::builder("password", NS_REGISTER).build());

    if include_email {
        query = query.append(Element::builder("email", NS_REGISTER).build());
    }

    Iq::Result {
        from: None,
        to: None,
        id: request_id.to_string(),
        payload: Some(query.build()),
    }
}

/// Build a registration success response.
pub fn build_registration_success(request_id: &str) -> Iq {
    Iq::Result {
        from: None,
        to: None,
        id: request_id.to_string(),
        payload: None,
    }
}

/// Build a registration error response.
pub fn build_registration_error(
    request_id: &str,
    submitted_query: Option<&Element>,
    error: &RegistrationError,
) -> RegistrationErrorResponse {
    let shape = registration_error_shape(error);
    RegistrationErrorResponse {
        request_id: request_id.to_string(),
        error_type: shape.error_type,
        condition: shape.condition,
        legacy_code: shape.legacy_code,
        submitted_query: submitted_query
            .cloned()
            .unwrap_or_else(|| Element::builder("query", NS_REGISTER).build()),
        text: registration_error_text(error).map(str::to_string),
    }
}

struct RegistrationErrorShape {
    error_type: ErrorType,
    condition: RegistrationErrorCondition,
    legacy_code: LegacyRegistrationErrorCode,
}

fn registration_error_shape(error: &RegistrationError) -> RegistrationErrorShape {
    match error {
        RegistrationError::NotAllowed => RegistrationErrorShape {
            error_type: ErrorType::Cancel,
            condition: RegistrationErrorCondition::ServiceUnavailable,
            legacy_code: LegacyRegistrationErrorCode::ServiceUnavailable,
        },
        RegistrationError::Conflict => RegistrationErrorShape {
            error_type: ErrorType::Cancel,
            condition: RegistrationErrorCondition::Conflict,
            legacy_code: LegacyRegistrationErrorCode::Conflict,
        },
        RegistrationError::NotAcceptable(_) => RegistrationErrorShape {
            error_type: ErrorType::Modify,
            condition: RegistrationErrorCondition::NotAcceptable,
            legacy_code: LegacyRegistrationErrorCode::NotAcceptable,
        },
        RegistrationError::BadRequest(_) => RegistrationErrorShape {
            error_type: ErrorType::Modify,
            condition: RegistrationErrorCondition::BadRequest,
            legacy_code: LegacyRegistrationErrorCode::BadRequest,
        },
        RegistrationError::InternalError(_) => RegistrationErrorShape {
            error_type: ErrorType::Wait,
            condition: RegistrationErrorCondition::InternalError,
            legacy_code: LegacyRegistrationErrorCode::InternalError,
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegistrationErrorCondition {
    BadRequest,
    Conflict,
    InternalError,
    NotAcceptable,
    ServiceUnavailable,
}

impl RegistrationErrorCondition {
    fn element_name(self) -> &'static str {
        match self {
            RegistrationErrorCondition::BadRequest => "bad-request",
            RegistrationErrorCondition::Conflict => "conflict",
            RegistrationErrorCondition::InternalError => "internal-server-error",
            RegistrationErrorCondition::NotAcceptable => "not-acceptable",
            RegistrationErrorCondition::ServiceUnavailable => "service-unavailable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LegacyRegistrationErrorCode {
    BadRequest,
    Conflict,
    NotAcceptable,
    ServiceUnavailable,
    InternalError,
}

impl LegacyRegistrationErrorCode {
    fn as_str(self) -> &'static str {
        match self {
            LegacyRegistrationErrorCode::BadRequest => "400",
            LegacyRegistrationErrorCode::NotAcceptable => "406",
            LegacyRegistrationErrorCode::Conflict => "409",
            LegacyRegistrationErrorCode::InternalError => "500",
            LegacyRegistrationErrorCode::ServiceUnavailable => "503",
        }
    }
}

fn error_type_attr(error_type: &ErrorType) -> &'static str {
    match error_type {
        ErrorType::Auth => "auth",
        ErrorType::Cancel => "cancel",
        ErrorType::Continue => "continue",
        ErrorType::Modify => "modify",
        ErrorType::Wait => "wait",
    }
}

fn registration_error_text(error: &RegistrationError) -> Option<&str> {
    match error {
        RegistrationError::NotAcceptable(msg)
        | RegistrationError::BadRequest(msg)
        | RegistrationError::InternalError(msg) => Some(msg),
        _ => None,
    }
}

/// Build registration feature advertisement for stream features.
///
/// Per XEP-0077 Section 8, this uses the feature namespace, not the IQ namespace.
pub fn build_registration_feature() -> Element {
    Element::builder("register", NS_REGISTER_FEATURE).build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_registration_query_element() {
        // Valid registration query (get)
        let xml = r#"<iq type='get' id='reg1' xmlns='jabber:client'><query xmlns='jabber:iq:register'/></iq>"#;
        let element: Element = xml.parse().unwrap();
        assert!(is_registration_query_element(&element));

        // Valid registration query (set)
        let xml = r#"<iq type='set' id='reg2' xmlns='jabber:client'><query xmlns='jabber:iq:register'><username>alice</username><password>secret</password></query></iq>"#;
        let element: Element = xml.parse().unwrap();
        assert!(is_registration_query_element(&element));

        // Not a registration query (different namespace)
        let xml = r#"<iq type='get' id='disco1' xmlns='jabber:client'><query xmlns='http://jabber.org/protocol/disco#info'/></iq>"#;
        let element: Element = xml.parse().unwrap();
        assert!(!is_registration_query_element(&element));

        // Not an IQ stanza
        let xml = r#"<message xmlns='jabber:client'><body>Hello</body></message>"#;
        let element: Element = xml.parse().unwrap();
        assert!(!is_registration_query_element(&element));
    }

    #[test]
    fn test_parse_registration_get() {
        let xml = r#"<iq type='get' id='reg1' xmlns='jabber:client'><query xmlns='jabber:iq:register'/></iq>"#;
        let element: Element = xml.parse().unwrap();
        let result = parse_registration_element(&element, "reg1").unwrap();
        assert!(result.is_none()); // Get request returns None
    }

    #[test]
    fn test_parse_registration_set() {
        let xml = r#"<iq type='set' id='reg2' xmlns='jabber:client'><query xmlns='jabber:iq:register'><username>alice</username><password>secret123</password><email>alice@example.com</email></query></iq>"#;
        let element: Element = xml.parse().unwrap();
        let result = parse_registration_element(&element, "reg2").unwrap();

        let request = result.expect("Should have registration request");
        assert_eq!(request.username, "alice");
        assert_eq!(request.password, "secret123");
        assert_eq!(request.email, Some("alice@example.com".to_string()));
    }

    #[test]
    fn test_parse_registration_set_no_email() {
        let xml = r#"<iq type='set' id='reg3' xmlns='jabber:client'><query xmlns='jabber:iq:register'><username>bob</username><password>pass</password></query></iq>"#;
        let element: Element = xml.parse().unwrap();
        let result = parse_registration_element(&element, "reg3").unwrap();

        let request = result.expect("Should have registration request");
        assert_eq!(request.username, "bob");
        assert_eq!(request.password, "pass");
        assert!(request.email.is_none());
    }

    #[test]
    fn test_parse_registration_missing_username() {
        let xml = r#"<iq type='set' id='reg4' xmlns='jabber:client'><query xmlns='jabber:iq:register'><password>secret</password></query></iq>"#;
        let element: Element = xml.parse().unwrap();
        let result = parse_registration_element(&element, "reg4");

        assert!(matches!(result, Err(RegistrationError::NotAcceptable(_))));
    }

    #[test]
    fn test_parse_registration_missing_password() {
        let xml = r#"<iq type='set' id='reg5' xmlns='jabber:client'><query xmlns='jabber:iq:register'><username>alice</username></query></iq>"#;
        let element: Element = xml.parse().unwrap();
        let result = parse_registration_element(&element, "reg5");

        assert!(matches!(result, Err(RegistrationError::NotAcceptable(_))));
    }
}

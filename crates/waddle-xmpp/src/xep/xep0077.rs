//! XEP-0077: In-Band Registration
//!
//! This module implements XEP-0077 which allows users to register accounts
//! directly through the XMPP connection, before authentication.
//!
//! ## Protocol Flow
//!
//! 1. Client requests registration form:
//!    ```xml
//!    <iq type='get' id='reg1'>
//!      <query xmlns='jabber:iq:register'/>
//!    </iq>
//!    ```
//!
//! 2. Server responds with required fields:
//!    ```xml
//!    <iq type='result' id='reg1'>
//!      <query xmlns='jabber:iq:register'>
//!        <instructions>Choose a username and password.</instructions>
//!        <username/>
//!        <password/>
//!        <email/>
//!      </query>
//!    </iq>
//!    ```
//!
//! 3. Client submits registration:
//!    ```xml
//!    <iq type='set' id='reg2'>
//!      <query xmlns='jabber:iq:register'>
//!        <username>alice</username>
//!        <password>secret</password>
//!        <email>alice@example.com</email>
//!      </query>
//!    </iq>
//!    ```
//!
//! 4. Server responds with success or error.
//!
//! ## Security Considerations
//!
//! - Registration should only be allowed over TLS connections
//! - Rate limiting should be implemented to prevent abuse
//! - Password requirements should be enforced

use minidom::Element;
use tracing::debug;
use xmpp_parsers::iq::Iq;

/// Namespace for XEP-0077 In-Band Registration
pub const NS_REGISTER: &str = "jabber:iq:register";

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
    parse_registration_element(&element, iq.id.as_str())
}

/// Parse a registration element (for pre-auth parsing where we have raw Element).
pub fn parse_registration_element(element: &Element, id: &str) -> Result<Option<RegistrationRequest>, RegistrationError> {
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
) -> String {
    let instructions_xml = instructions
        .map(|i| format!("<instructions>{}</instructions>", escape_xml(i)))
        .unwrap_or_default();

    let email_xml = if include_email { "<email/>" } else { "" };

    format!(
        "<iq type='result' id='{}'>\
            <query xmlns='{}'>\
                {}\
                <username/>\
                <password/>\
                {}\
            </query>\
        </iq>",
        escape_xml(request_id),
        NS_REGISTER,
        instructions_xml,
        email_xml
    )
}

/// Build a registration success response.
pub fn build_registration_success(request_id: &str) -> String {
    format!(
        "<iq type='result' id='{}'/>",
        escape_xml(request_id)
    )
}

/// Build a registration error response.
pub fn build_registration_error(request_id: &str, error: &RegistrationError) -> String {
    let (error_type, condition) = match error {
        RegistrationError::NotAllowed => ("cancel", "not-allowed"),
        RegistrationError::Conflict => ("cancel", "conflict"),
        RegistrationError::NotAcceptable(_) => ("modify", "not-acceptable"),
        RegistrationError::BadRequest(_) => ("modify", "bad-request"),
        RegistrationError::InternalError(_) => ("wait", "internal-server-error"),
    };

    let text = match error {
        RegistrationError::NotAcceptable(msg) | RegistrationError::BadRequest(msg) | RegistrationError::InternalError(msg) => {
            format!("<text xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'>{}</text>", escape_xml(msg))
        }
        _ => String::new(),
    };

    format!(
        "<iq type='error' id='{}'>\
            <query xmlns='{}'/>\
            <error type='{}'>\
                <{} xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/>\
                {}\
            </error>\
        </iq>",
        escape_xml(request_id),
        NS_REGISTER,
        error_type,
        condition,
        text
    )
}

/// Build registration feature advertisement for stream features.
///
/// This is included in stream features to indicate that registration is available.
pub fn build_registration_feature() -> String {
    format!("<register xmlns='{}'/>", NS_REGISTER)
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

    #[test]
    fn test_build_registration_fields_response() {
        let response = build_registration_fields_response("reg1", Some("Choose a username and password."), true);

        assert!(response.contains("type='result'"));
        assert!(response.contains("id='reg1'"));
        assert!(response.contains(&format!("xmlns='{}'", NS_REGISTER)));
        assert!(response.contains("<username/>"));
        assert!(response.contains("<password/>"));
        assert!(response.contains("<email/>"));
        assert!(response.contains("<instructions>Choose a username and password.</instructions>"));
    }

    #[test]
    fn test_build_registration_fields_response_no_email() {
        let response = build_registration_fields_response("reg1", None, false);

        assert!(response.contains("<username/>"));
        assert!(response.contains("<password/>"));
        assert!(!response.contains("<email/>"));
        assert!(!response.contains("<instructions>"));
    }

    #[test]
    fn test_build_registration_success() {
        let response = build_registration_success("reg2");

        assert!(response.contains("type='result'"));
        assert!(response.contains("id='reg2'"));
    }

    #[test]
    fn test_build_registration_error_conflict() {
        let response = build_registration_error("reg3", &RegistrationError::Conflict);

        assert!(response.contains("type='error'"));
        assert!(response.contains("id='reg3'"));
        assert!(response.contains("<conflict"));
    }

    #[test]
    fn test_build_registration_error_not_allowed() {
        let response = build_registration_error("reg4", &RegistrationError::NotAllowed);

        assert!(response.contains("type='error'"));
        assert!(response.contains("<not-allowed"));
    }

    #[test]
    fn test_build_registration_error_with_text() {
        let response = build_registration_error("reg5", &RegistrationError::NotAcceptable("Username is required".to_string()));

        assert!(response.contains("type='error'"));
        assert!(response.contains("<not-acceptable"));
        assert!(response.contains("<text"));
        assert!(response.contains("Username is required"));
    }

    #[test]
    fn test_build_registration_feature() {
        let feature = build_registration_feature();

        assert!(feature.contains(&format!("xmlns='{}'", NS_REGISTER)));
        assert!(feature.contains("<register"));
    }

    #[test]
    fn test_escape_xml() {
        assert_eq!(escape_xml("hello"), "hello");
        assert_eq!(escape_xml("<script>"), "&lt;script&gt;");
        assert_eq!(escape_xml("a & b"), "a &amp; b");
        assert_eq!(escape_xml("\"quoted\""), "&quot;quoted&quot;");
    }
}

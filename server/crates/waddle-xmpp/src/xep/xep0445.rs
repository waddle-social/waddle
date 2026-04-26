//! XEP-0445: Pre-Authenticated In-Band Registration
//!
//! Extends XEP-0077 (In-Band Registration) with invite token support.
//! New users register using a pre-authenticated token, bypassing
//! open registration while enabling invite-based onboarding.
//!
//! ## XML Format
//!
//! Registration with invite token:
//! ```xml
//! <iq type='set' to='example.com' id='reg-1'>
//!   <query xmlns='jabber:iq:register'>
//!     <username>newuser</username>
//!     <password>secret</password>
//!     <preauth xmlns='urn:xmpp:pars:0' token='invite-token-abc'/>
//!   </query>
//! </iq>
//! ```
//!
//! ## Use Cases
//!
//! - Closed registration with invite-only access
//! - Seamless onboarding from invite links
//! - Token validation during account creation

use minidom::Element;

/// Namespace for XEP-0445 Pre-Authenticated Registration.
pub const NS_PARS: &str = "urn:xmpp:pars:0";

/// A pre-authentication token in a registration request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreauthToken {
    /// The invite token string.
    pub token: String,
}

impl PreauthToken {
    /// Create a new preauth token.
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
        }
    }
}

impl std::fmt::Display for PreauthToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.token)
    }
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if an element is a `<preauth/>` element.
pub fn is_preauth_element(elem: &Element) -> bool {
    elem.ns() == NS_PARS && elem.name() == "preauth"
}

/// Check if a registration query contains a preauth token.
pub fn has_preauth(query_elem: &Element) -> bool {
    query_elem.children().any(is_preauth_element)
}

// ── Extraction ───────────────────────────────────────────────────────

/// Extract a preauth token from a registration query element.
pub fn extract_preauth(query_elem: &Element) -> Option<PreauthToken> {
    query_elem
        .children()
        .find_map(|elem| is_preauth_element(elem).then(|| elem.attr("token")).flatten())
        .filter(|t| !t.is_empty())
        .map(PreauthToken::new)
}

// ── Building ─────────────────────────────────────────────────────────

/// Build a `<preauth xmlns='urn:xmpp:pars:0' token='...'/>` element.
pub fn build_preauth_element(token: &str) -> Element {
    Element::builder("preauth", NS_PARS)
        .attr("token", token)
        .build()
}

/// Validation result for a preauth token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreauthValidation {
    /// Token is valid, registration can proceed.
    Valid,
    /// Token is not recognized.
    InvalidToken,
    /// Token has already been used.
    AlreadyUsed,
    /// Token has expired.
    Expired,
    /// No token provided but registration requires one.
    Required,
}

impl PreauthValidation {
    /// Returns `true` if the token is valid.
    pub fn is_valid(&self) -> bool {
        matches!(self, Self::Valid)
    }

    /// Returns an error message if invalid.
    pub fn error_message(&self) -> Option<&'static str> {
        match self {
            Self::Valid => None,
            Self::InvalidToken => Some("Invalid invitation token"),
            Self::AlreadyUsed => Some("Invitation token has already been used"),
            Self::Expired => Some("Invitation token has expired"),
            Self::Required => Some("An invitation token is required to register"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_preauth_element() {
        let elem = Element::builder("preauth", NS_PARS)
            .attr("token", "abc")
            .build();
        assert!(is_preauth_element(&elem));

        let wrong = Element::builder("preauth", "jabber:client").build();
        assert!(!is_preauth_element(&wrong));
    }

    #[test]
    fn test_has_preauth() {
        let mut query = Element::builder("query", "jabber:iq:register").build();
        assert!(!has_preauth(&query));

        query.append_child(build_preauth_element("tok-1"));
        assert!(has_preauth(&query));
    }

    #[test]
    fn test_extract_preauth() {
        let mut query = Element::builder("query", "jabber:iq:register").build();
        query.append_child(build_preauth_element("invite-abc"));

        let token = extract_preauth(&query).expect("has token");
        assert_eq!(token.token, "invite-abc");
    }

    #[test]
    fn test_extract_preauth_absent() {
        let query = Element::builder("query", "jabber:iq:register").build();
        assert!(extract_preauth(&query).is_none());
    }

    #[test]
    fn test_extract_preauth_empty_token() {
        let mut query = Element::builder("query", "jabber:iq:register").build();
        query.append_child(build_preauth_element(""));
        assert!(extract_preauth(&query).is_none());
    }

    #[test]
    fn test_build_preauth_element() {
        let elem = build_preauth_element("tok-123");
        assert_eq!(elem.name(), "preauth");
        assert_eq!(elem.ns(), NS_PARS);
        assert_eq!(elem.attr("token"), Some("tok-123"));
    }

    #[test]
    fn test_preauth_token_display() {
        let t = PreauthToken::new("display-test");
        assert_eq!(t.to_string(), "display-test");
    }

    #[test]
    fn test_preauth_validation() {
        assert!(PreauthValidation::Valid.is_valid());
        assert!(PreauthValidation::Valid.error_message().is_none());

        assert!(!PreauthValidation::InvalidToken.is_valid());
        assert!(PreauthValidation::InvalidToken.error_message().is_some());

        assert!(!PreauthValidation::AlreadyUsed.is_valid());
        assert!(!PreauthValidation::Expired.is_valid());
        assert!(!PreauthValidation::Required.is_valid());
        assert_eq!(
            PreauthValidation::Required.error_message(),
            Some("An invitation token is required to register")
        );
    }

    #[test]
    fn test_is_preauth_element_negative() {
        let wrong = Element::builder("other", NS_PARS).build();
        assert!(!is_preauth_element(&wrong));
        let wrong_ns = Element::builder("preauth", "wrong:ns").build();
        assert!(!is_preauth_element(&wrong_ns));
    }

    #[test]
    fn test_namespace_constant() {
        assert_eq!(NS_PARS, "urn:xmpp:pars:0");
    }

    #[test]
    fn test_preauth_token_new() {
        let t = PreauthToken::new("abc");
        assert_eq!(t.token, "abc");
    }
}

//! XEP-0249: Direct MUC Invitations
//!
//! Provides support for inviting users directly to MUC rooms using a simple
//! message-based invitation mechanism. This is an alternative to the mediated
//! invitations defined in XEP-0045.
//!
//! ## Overview
//!
//! Direct MUC invitations allow a user to invite another user to a MUC room
//! by sending them a message with a special `<x>` element containing:
//! - The JID of the room to join (required)
//! - An optional reason for the invitation
//! - An optional password if the room is password-protected
//!
//! ## XML Format
//!
//! ```xml
//! <message from='crone1@shakespeare.lit/desktop'
//!          to='hecate@shakespeare.lit'>
//!   <x xmlns='jabber:x:conference'
//!      jid='darkcave@macbeth.shakespeare.lit'
//!      reason='Hey Hecate, this is the place for all good witches!'
//!      password='cauldronburn'/>
//! </message>
//! ```

use jid::BareJid;
use minidom::Element;
use tracing::debug;

/// Namespace for XEP-0249 Direct MUC Invitations.
pub const NS_CONFERENCE: &str = "jabber:x:conference";

/// A direct MUC invitation parsed from a message stanza.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectInvite {
    /// The JID of the MUC room to join (required).
    pub jid: BareJid,
    /// Optional reason/message for the invitation.
    pub reason: Option<String>,
    /// Optional password for password-protected rooms.
    pub password: Option<String>,
}

impl DirectInvite {
    /// Create a new direct invite with just a room JID.
    pub fn new(jid: BareJid) -> Self {
        Self {
            jid,
            reason: None,
            password: None,
        }
    }

    /// Create a new direct invite with a room JID and reason.
    pub fn with_reason(jid: BareJid, reason: impl Into<String>) -> Self {
        Self {
            jid,
            reason: Some(reason.into()),
            password: None,
        }
    }

    /// Create a new direct invite with all fields.
    pub fn with_password(
        jid: BareJid,
        reason: Option<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            jid,
            reason,
            password: Some(password.into()),
        }
    }

    /// Set the reason for the invitation.
    pub fn set_reason(&mut self, reason: impl Into<String>) {
        self.reason = Some(reason.into());
    }

    /// Set the password for the invitation.
    pub fn set_password(&mut self, password: impl Into<String>) {
        self.password = Some(password.into());
    }
}

/// Check if a message element contains a direct MUC invitation (XEP-0249).
///
/// Returns true if the message contains an `<x xmlns='jabber:x:conference'>` child element.
pub fn is_direct_invite(element: &Element) -> bool {
    element.get_child("x", NS_CONFERENCE).is_some()
}

/// Check if a parsed Message contains a direct MUC invitation.
pub fn message_has_direct_invite(msg: &xmpp_parsers::message::Message) -> bool {
    msg.payloads
        .iter()
        .any(|p| p.name() == "x" && p.ns() == NS_CONFERENCE)
}

/// Parse a direct MUC invitation from a message element.
///
/// Returns `Some(DirectInvite)` if the message contains a valid invitation,
/// or `None` if no invitation is found or the invitation is malformed.
pub fn parse_direct_invite(element: &Element) -> Option<DirectInvite> {
    let x_elem = element.get_child("x", NS_CONFERENCE)?;
    parse_invite_element(x_elem)
}

/// Parse a direct MUC invitation from a Message.
pub fn parse_direct_invite_from_message(
    msg: &xmpp_parsers::message::Message,
) -> Option<DirectInvite> {
    for payload in &msg.payloads {
        if payload.name() == "x" && payload.ns() == NS_CONFERENCE {
            return parse_invite_element(payload);
        }
    }
    None
}

/// Parse the `<x>` element into a DirectInvite.
fn parse_invite_element(x_elem: &Element) -> Option<DirectInvite> {
    // The jid attribute is required
    let jid_str = x_elem.attr("jid")?;
    let jid: BareJid = jid_str.parse().ok()?;

    // Reason and password are optional
    let reason = x_elem
        .attr("reason")
        .filter(|s| !s.is_empty())
        .map(String::from);
    let password = x_elem
        .attr("password")
        .filter(|s| !s.is_empty())
        .map(String::from);

    debug!(
        room = %jid,
        has_reason = reason.is_some(),
        has_password = password.is_some(),
        "Parsed direct MUC invitation"
    );

    Some(DirectInvite {
        jid,
        reason,
        password,
    })
}

/// Build a direct invite `<x>` element from a DirectInvite struct.
///
/// The resulting element can be added to a message stanza.
pub fn build_direct_invite(invite: &DirectInvite) -> Element {
    let mut builder = Element::builder("x", NS_CONFERENCE).attr("jid", invite.jid.to_string());

    if let Some(ref reason) = invite.reason {
        builder = builder.attr("reason", reason.as_str());
    }

    if let Some(ref password) = invite.password {
        builder = builder.attr("password", password.as_str());
    }

    builder.build()
}

/// Build a complete message stanza containing a direct MUC invitation.
///
/// # Arguments
///
/// * `from` - The JID of the user sending the invitation
/// * `to` - The JID of the user being invited
/// * `invite` - The invitation details
/// * `body` - Optional message body (some clients display this)
///
/// # Returns
///
/// An XML string representing the complete message stanza.
pub fn build_invite_message(
    from: &jid::Jid,
    to: &jid::Jid,
    invite: &DirectInvite,
    body: Option<&str>,
) -> String {
    let invite_elem = build_direct_invite(invite);
    let invite_xml = String::from(&invite_elem);

    let body_xml = body
        .map(|b| format!("<body>{}</body>", escape_xml(b)))
        .unwrap_or_default();

    format!(
        "<message from='{}' to='{}'>{}{}</message>",
        escape_xml(&from.to_string()),
        escape_xml(&to.to_string()),
        body_xml,
        invite_xml
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
    fn test_is_direct_invite() {
        // Valid direct invite
        let xml = r#"<message xmlns='jabber:client' from='user@example.com' to='friend@example.com'>
            <x xmlns='jabber:x:conference' jid='room@conference.example.com'/>
        </message>"#;
        let element: Element = xml.parse().unwrap();
        assert!(is_direct_invite(&element));

        // Message without invite
        let xml = r#"<message xmlns='jabber:client' from='user@example.com' to='friend@example.com'>
            <body>Hello!</body>
        </message>"#;
        let element: Element = xml.parse().unwrap();
        assert!(!is_direct_invite(&element));

        // Wrong namespace
        let xml = r#"<message xmlns='jabber:client'>
            <x xmlns='wrong:namespace' jid='room@conference.example.com'/>
        </message>"#;
        let element: Element = xml.parse().unwrap();
        assert!(!is_direct_invite(&element));
    }

    #[test]
    fn test_parse_direct_invite_minimal() {
        let xml = r#"<message xmlns='jabber:client'>
            <x xmlns='jabber:x:conference' jid='darkcave@macbeth.shakespeare.lit'/>
        </message>"#;
        let element: Element = xml.parse().unwrap();

        let invite = parse_direct_invite(&element).unwrap();
        assert_eq!(invite.jid.to_string(), "darkcave@macbeth.shakespeare.lit");
        assert!(invite.reason.is_none());
        assert!(invite.password.is_none());
    }

    #[test]
    fn test_parse_direct_invite_with_reason() {
        let xml = r#"<message xmlns='jabber:client'>
            <x xmlns='jabber:x:conference'
               jid='darkcave@macbeth.shakespeare.lit'
               reason='Hey Hecate, this is the place for all good witches!'/>
        </message>"#;
        let element: Element = xml.parse().unwrap();

        let invite = parse_direct_invite(&element).unwrap();
        assert_eq!(invite.jid.to_string(), "darkcave@macbeth.shakespeare.lit");
        assert_eq!(
            invite.reason.as_deref(),
            Some("Hey Hecate, this is the place for all good witches!")
        );
        assert!(invite.password.is_none());
    }

    #[test]
    fn test_parse_direct_invite_full() {
        let xml = r#"<message xmlns='jabber:client'>
            <x xmlns='jabber:x:conference'
               jid='darkcave@macbeth.shakespeare.lit'
               reason='Hey Hecate, this is the place for all good witches!'
               password='cauldronburn'/>
        </message>"#;
        let element: Element = xml.parse().unwrap();

        let invite = parse_direct_invite(&element).unwrap();
        assert_eq!(invite.jid.to_string(), "darkcave@macbeth.shakespeare.lit");
        assert_eq!(
            invite.reason.as_deref(),
            Some("Hey Hecate, this is the place for all good witches!")
        );
        assert_eq!(invite.password.as_deref(), Some("cauldronburn"));
    }

    #[test]
    fn test_parse_direct_invite_missing_jid() {
        // Missing required jid attribute
        let xml = r#"<message xmlns='jabber:client'>
            <x xmlns='jabber:x:conference' reason='Join us!'/>
        </message>"#;
        let element: Element = xml.parse().unwrap();

        assert!(parse_direct_invite(&element).is_none());
    }

    #[test]
    fn test_parse_direct_invite_invalid_jid() {
        // Empty JID should fail
        let xml = r#"<message xmlns='jabber:client'>
            <x xmlns='jabber:x:conference' jid=''/>
        </message>"#;
        let element: Element = xml.parse().unwrap();
        assert!(parse_direct_invite(&element).is_none());

        // JID with only @ sign should fail
        let xml = r#"<message xmlns='jabber:client'>
            <x xmlns='jabber:x:conference' jid='@'/>
        </message>"#;
        let element: Element = xml.parse().unwrap();
        assert!(parse_direct_invite(&element).is_none());
    }

    #[test]
    fn test_parse_direct_invite_empty_reason() {
        // Empty reason should be treated as None
        let xml = r#"<message xmlns='jabber:client'>
            <x xmlns='jabber:x:conference' jid='room@conference.example.com' reason=''/>
        </message>"#;
        let element: Element = xml.parse().unwrap();

        let invite = parse_direct_invite(&element).unwrap();
        assert!(invite.reason.is_none());
    }

    #[test]
    fn test_build_direct_invite_minimal() {
        let jid: BareJid = "room@conference.example.com".parse().unwrap();
        let invite = DirectInvite::new(jid);

        let elem = build_direct_invite(&invite);

        assert_eq!(elem.name(), "x");
        assert_eq!(elem.ns(), NS_CONFERENCE);
        assert_eq!(elem.attr("jid"), Some("room@conference.example.com"));
        assert!(elem.attr("reason").is_none());
        assert!(elem.attr("password").is_none());
    }

    #[test]
    fn test_build_direct_invite_with_reason() {
        let jid: BareJid = "room@conference.example.com".parse().unwrap();
        let invite = DirectInvite::with_reason(jid, "Join our chat!");

        let elem = build_direct_invite(&invite);

        assert_eq!(elem.attr("jid"), Some("room@conference.example.com"));
        assert_eq!(elem.attr("reason"), Some("Join our chat!"));
        assert!(elem.attr("password").is_none());
    }

    #[test]
    fn test_build_direct_invite_full() {
        let jid: BareJid = "room@conference.example.com".parse().unwrap();
        let invite = DirectInvite::with_password(jid, Some("Join us!".to_string()), "secret123");

        let elem = build_direct_invite(&invite);

        assert_eq!(elem.attr("jid"), Some("room@conference.example.com"));
        assert_eq!(elem.attr("reason"), Some("Join us!"));
        assert_eq!(elem.attr("password"), Some("secret123"));
    }

    #[test]
    fn test_build_invite_message() {
        let from: jid::Jid = "crone1@shakespeare.lit/desktop".parse().unwrap();
        let to: jid::Jid = "hecate@shakespeare.lit".parse().unwrap();
        let jid: BareJid = "darkcave@macbeth.shakespeare.lit".parse().unwrap();
        let invite = DirectInvite::with_reason(jid, "Join us!");

        let msg = build_invite_message(&from, &to, &invite, None);

        // Check the message wrapper has proper from/to attributes
        assert!(msg.contains("from='crone1@shakespeare.lit/desktop'"));
        assert!(msg.contains("to='hecate@shakespeare.lit'"));
        // Check the invite element is present with correct namespace and attributes
        assert!(msg.contains("jabber:x:conference"));
        assert!(msg.contains("darkcave@macbeth.shakespeare.lit"));
        assert!(msg.contains("Join us!"));
    }

    #[test]
    fn test_build_invite_message_with_body() {
        let from: jid::Jid = "user@example.com".parse().unwrap();
        let to: jid::Jid = "friend@example.com".parse().unwrap();
        let jid: BareJid = "room@conference.example.com".parse().unwrap();
        let invite = DirectInvite::new(jid);

        let msg = build_invite_message(
            &from,
            &to,
            &invite,
            Some("You've been invited to join a room!"),
        );

        assert!(msg.contains("<body>You&apos;ve been invited to join a room!</body>"));
        assert!(msg.contains("xmlns='jabber:x:conference'"));
    }

    #[test]
    fn test_direct_invite_setters() {
        let jid: BareJid = "room@conference.example.com".parse().unwrap();
        let mut invite = DirectInvite::new(jid);

        assert!(invite.reason.is_none());
        assert!(invite.password.is_none());

        invite.set_reason("Come join!");
        assert_eq!(invite.reason.as_deref(), Some("Come join!"));

        invite.set_password("secret");
        assert_eq!(invite.password.as_deref(), Some("secret"));
    }

    #[test]
    fn test_roundtrip() {
        let jid: BareJid = "room@conference.example.com".parse().unwrap();
        let original =
            DirectInvite::with_password(jid, Some("Test roundtrip".to_string()), "testpass");

        // Build the element
        let elem = build_direct_invite(&original);

        // Wrap in a message for parsing
        let msg = Element::builder("message", "jabber:client")
            .append(elem)
            .build();

        // Parse it back
        let parsed = parse_direct_invite(&msg).unwrap();

        assert_eq!(original, parsed);
    }

    #[test]
    fn test_escape_special_characters() {
        let from: jid::Jid = "user@example.com".parse().unwrap();
        let to: jid::Jid = "friend@example.com".parse().unwrap();
        let jid: BareJid = "room@conference.example.com".parse().unwrap();
        let mut invite = DirectInvite::new(jid);
        invite.set_reason("Join <us> & have fun!");

        let msg = build_invite_message(&from, &to, &invite, Some("Check this <out>!"));

        // Verify special characters are escaped in body
        assert!(msg.contains("Check this &lt;out&gt;!"));
        // Note: The element builder handles attribute escaping, so reason may appear differently
    }
}

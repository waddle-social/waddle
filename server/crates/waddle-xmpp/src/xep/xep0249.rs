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
use waddle_xmpp_core::mam::ThreadId;
use xmpp_parsers::message::{Message, MessageType};

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
    /// Whether the room continues an existing one-to-one chat.
    pub continue_chat: bool,
    /// Optional one-to-one chat thread being continued.
    pub thread: Option<ThreadId>,
}

impl DirectInvite {
    /// Create a new direct invite with just a room JID.
    pub fn new(jid: BareJid) -> Self {
        Self {
            jid,
            reason: None,
            password: None,
            continue_chat: false,
            thread: None,
        }
    }

    /// Create a new direct invite with a room JID and reason.
    pub fn with_reason(jid: BareJid, reason: impl Into<String>) -> Self {
        Self {
            jid,
            reason: Some(reason.into()),
            password: None,
            continue_chat: false,
            thread: None,
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
            continue_chat: false,
            thread: None,
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

    /// Mark the invitation as a continuation of a one-to-one chat.
    pub fn set_continue(&mut self, thread: Option<ThreadId>) {
        self.continue_chat = true;
        self.thread = thread;
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
    let continue_chat = match x_elem.attr("continue") {
        Some("true") | Some("1") => true,
        Some("false") | Some("0") | None => false,
        Some(_) => return None,
    };
    let thread = x_elem
        .attr("thread")
        .filter(|s| !s.is_empty())
        .and_then(ThreadId::new);

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
        continue_chat,
        thread,
    })
}

/// Build a direct invite `<x>` element from a DirectInvite struct.
///
/// The resulting element can be added to a message stanza.
pub fn build_direct_invite(invite: &DirectInvite) -> Element {
    let mut builder = Element::builder("x", NS_CONFERENCE).attr(
        minidom::rxml::xml_ncname!("jid").to_owned(),
        invite.jid.to_string(),
    );

    if let Some(ref reason) = invite.reason {
        builder = builder.attr(
            minidom::rxml::xml_ncname!("reason").to_owned(),
            reason.as_str(),
        );
    }

    if let Some(ref password) = invite.password {
        builder = builder.attr(
            minidom::rxml::xml_ncname!("password").to_owned(),
            password.as_str(),
        );
    }

    if invite.continue_chat {
        builder = builder.attr(minidom::rxml::xml_ncname!("continue").to_owned(), "true");
    }

    if let Some(ref thread) = invite.thread {
        builder = builder.attr(
            minidom::rxml::xml_ncname!("thread").to_owned(),
            thread.as_str(),
        );
    }

    builder.build()
}

/// Build a typed message stanza containing a direct MUC invitation.
///
/// # Arguments
///
/// * `from` - The JID of the user sending the invitation
/// * `to` - The JID of the user being invited
/// * `invite` - The invitation details
///
/// # Returns
///
/// A typed message stanza. XML serialization happens at the transport boundary.
pub fn build_invite_message(from: &jid::Jid, to: &jid::Jid, invite: &DirectInvite) -> Message {
    let mut msg = Message::new_with_type(MessageType::Normal, Some(to.clone()));
    msg.from = Some(from.clone());
    msg.payloads.push(build_direct_invite(invite));
    msg
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
               continue='true'
               reason='Hey Hecate, this is the place for all good witches!'
               password='cauldronburn'
               thread='e0ffe42b28561960c6b12b944a092794b9683a38'/>
        </message>"#;
        let element: Element = xml.parse().unwrap();

        let invite = parse_direct_invite(&element).unwrap();
        assert_eq!(invite.jid.to_string(), "darkcave@macbeth.shakespeare.lit");
        assert_eq!(
            invite.reason.as_deref(),
            Some("Hey Hecate, this is the place for all good witches!")
        );
        assert_eq!(invite.password.as_deref(), Some("cauldronburn"));
        assert!(invite.continue_chat);
        assert_eq!(
            invite.thread.as_ref().map(ThreadId::as_str),
            Some("e0ffe42b28561960c6b12b944a092794b9683a38")
        );
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
    fn test_parse_direct_invite_invalid_continue() {
        let xml = r#"<message xmlns='jabber:client'>
            <x xmlns='jabber:x:conference'
               jid='room@conference.example.com'
               continue='maybe'/>
        </message>"#;
        let element: Element = xml.parse().unwrap();

        assert!(parse_direct_invite(&element).is_none());
    }

    #[test]
    fn test_parse_direct_invite_false_continue() {
        let xml = r#"<message xmlns='jabber:client'>
            <x xmlns='jabber:x:conference'
               jid='room@conference.example.com'
               continue='false'/>
        </message>"#;
        let element: Element = xml.parse().unwrap();

        let invite = parse_direct_invite(&element).unwrap();
        assert!(!invite.continue_chat);
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
        let password = uuid::Uuid::new_v4().to_string();
        let invite = DirectInvite::with_password(jid, Some("Join us!".to_string()), &password);

        let elem = build_direct_invite(&invite);

        assert_eq!(elem.attr("jid"), Some("room@conference.example.com"));
        assert_eq!(elem.attr("reason"), Some("Join us!"));
        assert_eq!(elem.attr("password"), Some(password.as_str()));
        assert!(elem.attr("continue").is_none());
        assert!(elem.attr("thread").is_none());
    }

    #[test]
    fn test_build_direct_invite_continue_thread() {
        let jid: BareJid = "room@conference.example.com".parse().unwrap();
        let mut invite = DirectInvite::new(jid);
        invite.set_continue(ThreadId::new("thread-1"));

        let elem = build_direct_invite(&invite);

        assert_eq!(elem.attr("continue"), Some("true"));
        assert_eq!(elem.attr("thread"), Some("thread-1"));
    }

    #[test]
    fn test_build_invite_message() {
        let from: jid::Jid = "crone1@shakespeare.lit/desktop".parse().unwrap();
        let to: jid::Jid = "hecate@shakespeare.lit".parse().unwrap();
        let jid: BareJid = "darkcave@macbeth.shakespeare.lit".parse().unwrap();
        let invite = DirectInvite::with_reason(jid, "Join us!");

        let msg = build_invite_message(&from, &to, &invite);

        assert_eq!(msg.from.as_ref(), Some(&from));
        assert_eq!(msg.to.as_ref(), Some(&to));
        assert_eq!(msg.type_, MessageType::Normal);
        assert!(msg.bodies.is_empty());

        let invite_elem = msg
            .payloads
            .iter()
            .find(|p| p.name() == "x" && p.ns() == NS_CONFERENCE)
            .expect("direct invite payload");
        assert_eq!(
            invite_elem.attr("jid"),
            Some("darkcave@macbeth.shakespeare.lit")
        );
        assert_eq!(invite_elem.attr("reason"), Some("Join us!"));
    }

    #[test]
    fn test_build_invite_message_has_no_body() {
        let from: jid::Jid = "user@example.com".parse().unwrap();
        let to: jid::Jid = "friend@example.com".parse().unwrap();
        let jid: BareJid = "room@conference.example.com".parse().unwrap();
        let invite = DirectInvite::new(jid);

        let msg = build_invite_message(&from, &to, &invite);

        assert!(msg.bodies.is_empty());
        assert!(message_has_direct_invite(&msg));
    }

    #[test]
    fn test_direct_invite_setters() {
        let jid: BareJid = "room@conference.example.com".parse().unwrap();
        let mut invite = DirectInvite::new(jid);

        assert!(invite.reason.is_none());
        assert!(invite.password.is_none());

        invite.set_reason("Come join!");
        assert_eq!(invite.reason.as_deref(), Some("Come join!"));

        let password = uuid::Uuid::new_v4().to_string();
        invite.set_password(&password);
        assert!(invite.password.is_some());
    }

    #[test]
    fn test_roundtrip() {
        let jid: BareJid = "room@conference.example.com".parse().unwrap();
        let original = DirectInvite::with_password(
            jid,
            Some("Test roundtrip".to_string()),
            uuid::Uuid::new_v4().to_string(),
        );

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

        let msg = build_invite_message(&from, &to, &invite);
        let elem = Element::from(msg.clone());
        let xml = String::from(&elem);

        assert!(xml.contains("Join &lt;us&gt; &amp; have fun!"));

        let parsed = parse_direct_invite_from_message(&msg).unwrap();
        assert_eq!(parsed.reason.as_deref(), Some("Join <us> & have fun!"));
    }

    #[test]
    fn test_xep0249_module_does_not_hand_build_xml() {
        let source = include_str!("xep0249.rs");
        let builder = source
            .split("pub fn build_invite_message")
            .nth(1)
            .and_then(|rest| rest.split("#[cfg(test)]").next())
            .expect("build_invite_message source");
        let forbidden_macro = ["format", "!"].join("");
        let forbidden_escape_helper = ["escape", "_xml"].join("");

        assert!(!builder.contains(&forbidden_escape_helper));
        assert!(!builder.contains(&forbidden_macro));
        assert!(!builder.contains("<message"));
        assert!(!builder.contains("<body"));
        assert!(!builder.contains("String::from"));
    }
}

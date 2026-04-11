//! XEP-0513: Explicit Mentions
//!
//! Defines structured mention semantics for MUC messages. Complements
//! XEP-0372 (References) with notification-relevant mention types:
//! individual @user, group @everyone/@here, and role-based mentions.
//!
//! ## XML Format
//!
//! ```xml
//! <message type='groupchat' to='room@muc.example.com'>
//!   <body>@everyone Meeting in 5 minutes!</body>
//!   <mentions xmlns='urn:xmpp:emn:0'>
//!     <mention type='everyone'/>
//!   </mentions>
//! </message>
//! ```
//!
//! Individual mention:
//! ```xml
//! <mentions xmlns='urn:xmpp:emn:0'>
//!   <mention type='jid' value='alice@example.com'/>
//! </mentions>
//! ```
//!
//! ## Mention Types
//!
//! - **jid**: Mention a specific user by JID
//! - **nick**: Mention a specific user by MUC nick
//! - **everyone**: Notify all room occupants
//! - **here**: Notify all currently online occupants
//! - **role**: Notify users with a specific role (admin, moderator)

use minidom::Element;
use xmpp_parsers::message::Message;

/// Namespace for XEP-0513 Explicit Mentions.
pub const NS_EXPLICIT_MENTIONS: &str = "urn:xmpp:emn:0";

/// The type of an explicit mention.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MentionType {
    /// Mention a specific user by bare JID.
    Jid(String),
    /// Mention a specific user by MUC nickname.
    Nick(String),
    /// Mention everyone in the room.
    Everyone,
    /// Mention everyone currently online.
    Here,
    /// Mention users with a specific role.
    Role(String),
}

impl MentionType {
    /// Returns `true` if this is a broadcast mention (@everyone or @here).
    pub fn is_broadcast(&self) -> bool {
        matches!(self, Self::Everyone | Self::Here)
    }

    /// Returns `true` if this targets a specific user.
    pub fn is_individual(&self) -> bool {
        matches!(self, Self::Jid(_) | Self::Nick(_))
    }

    /// Returns the type attribute string.
    pub fn type_str(&self) -> &str {
        match self {
            Self::Jid(_) => "jid",
            Self::Nick(_) => "nick",
            Self::Everyone => "everyone",
            Self::Here => "here",
            Self::Role(_) => "role",
        }
    }

    /// Returns the value attribute, if any.
    pub fn value(&self) -> Option<&str> {
        match self {
            Self::Jid(v) | Self::Nick(v) | Self::Role(v) => Some(v),
            Self::Everyone | Self::Here => None,
        }
    }
}

/// A set of explicit mentions in a message.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExplicitMentions {
    pub mentions: Vec<MentionType>,
}

impl ExplicitMentions {
    /// Create an empty mention set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a mention.
    pub fn with_mention(mut self, mention: MentionType) -> Self {
        self.mentions.push(mention);
        self
    }

    /// Add @everyone.
    pub fn with_everyone(self) -> Self {
        self.with_mention(MentionType::Everyone)
    }

    /// Add @here.
    pub fn with_here(self) -> Self {
        self.with_mention(MentionType::Here)
    }

    /// Add a JID mention.
    pub fn with_jid(self, jid: impl Into<String>) -> Self {
        self.with_mention(MentionType::Jid(jid.into()))
    }

    /// Add a nick mention.
    pub fn with_nick(self, nick: impl Into<String>) -> Self {
        self.with_mention(MentionType::Nick(nick.into()))
    }

    /// Returns `true` if @everyone is mentioned.
    pub fn mentions_everyone(&self) -> bool {
        self.mentions
            .iter()
            .any(|m| matches!(m, MentionType::Everyone))
    }

    /// Returns `true` if @here is mentioned.
    pub fn mentions_here(&self) -> bool {
        self.mentions.iter().any(|m| matches!(m, MentionType::Here))
    }

    /// Returns `true` if there's any broadcast mention.
    pub fn has_broadcast(&self) -> bool {
        self.mentions.iter().any(|m| m.is_broadcast())
    }

    /// Check if a specific JID is mentioned.
    pub fn mentions_jid(&self, jid: &str) -> bool {
        self.mentions
            .iter()
            .any(|m| matches!(m, MentionType::Jid(j) if j == jid))
    }

    /// Check if a specific nick is mentioned.
    pub fn mentions_nick(&self, nick: &str) -> bool {
        self.mentions
            .iter()
            .any(|m| matches!(m, MentionType::Nick(n) if n == nick))
    }

    /// Returns `true` if empty.
    pub fn is_empty(&self) -> bool {
        self.mentions.is_empty()
    }

    /// Should a given user be notified? Checks JID, nick, and broadcast.
    pub fn should_notify(&self, user_jid: &str, user_nick: &str) -> bool {
        self.has_broadcast() || self.mentions_jid(user_jid) || self.mentions_nick(user_nick)
    }
}

/// Trait for types that can carry explicit mentions.
pub trait ExplicitMentionCarrier {
    /// Extract explicit mentions from this carrier.
    fn explicit_mentions(&self) -> Option<ExplicitMentions>;

    /// Returns `true` if this has any explicit mentions.
    fn has_explicit_mentions(&self) -> bool {
        self.explicit_mentions().is_some_and(|m| !m.is_empty())
    }
}

impl ExplicitMentionCarrier for Message {
    fn explicit_mentions(&self) -> Option<ExplicitMentions> {
        extract_explicit_mentions(self)
    }
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if an element is a `<mentions/>` element.
pub fn is_mentions_element(elem: &Element) -> bool {
    elem.ns() == NS_EXPLICIT_MENTIONS && elem.name() == "mentions"
}

/// Check if a message has explicit mentions.
pub fn has_explicit_mentions(msg: &Message) -> bool {
    msg.payloads.iter().any(is_mentions_element)
}

// ── Extraction ───────────────────────────────────────────────────────

/// Extract explicit mentions from a message.
pub fn extract_explicit_mentions(msg: &Message) -> Option<ExplicitMentions> {
    let elem = msg.payloads.iter().find(|e| is_mentions_element(e))?;
    Some(parse_mentions_element(elem))
}

/// Parse a `<mentions/>` element.
pub fn parse_mentions_element(elem: &Element) -> ExplicitMentions {
    let mentions: Vec<MentionType> = elem
        .children()
        .filter(|c| c.name() == "mention" && c.ns() == NS_EXPLICIT_MENTIONS)
        .filter_map(|c| {
            let mt = c.attr("type")?;
            let value = c
                .attr("value")
                .filter(|v| !v.is_empty())
                .map(|v| v.to_owned());
            match mt {
                "jid" => Some(MentionType::Jid(value?)),
                "nick" => Some(MentionType::Nick(value?)),
                "everyone" => Some(MentionType::Everyone),
                "here" => Some(MentionType::Here),
                "role" => Some(MentionType::Role(value?)),
                _ => None,
            }
        })
        .collect();

    ExplicitMentions { mentions }
}

// ── Building ─────────────────────────────────────────────────────────

/// Build a `<mentions/>` element.
pub fn build_mentions_element(mentions: &ExplicitMentions) -> Element {
    let mut elem = Element::builder("mentions", NS_EXPLICIT_MENTIONS).build();

    for m in &mentions.mentions {
        let mut mention = Element::builder("mention", NS_EXPLICIT_MENTIONS)
            .attr("type", m.type_str())
            .build();
        if let Some(value) = m.value() {
            mention.set_attr("value", value);
        }
        elem.append_child(mention);
    }

    elem
}

// ── Mutation ─────────────────────────────────────────────────────────

/// Set explicit mentions on a message.
pub fn set_explicit_mentions(msg: &mut Message, mentions: &ExplicitMentions) {
    msg.payloads.retain(|e| e.ns() != NS_EXPLICIT_MENTIONS);
    if !mentions.is_empty() {
        msg.payloads.push(build_mentions_element(mentions));
    }
}

/// Remove explicit mentions from a message.
pub fn strip_explicit_mentions(msg: &mut Message) {
    msg.payloads.retain(|e| e.ns() != NS_EXPLICIT_MENTIONS);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_mentions_element() {
        let elem = Element::builder("mentions", NS_EXPLICIT_MENTIONS).build();
        assert!(is_mentions_element(&elem));

        let wrong = Element::builder("mentions", "jabber:client").build();
        assert!(!is_mentions_element(&wrong));
    }

    #[test]
    fn test_parse_everyone() {
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <body>@everyone hello!</body>\
                    <mentions xmlns='urn:xmpp:emn:0'>\
                      <mention type='everyone'/>\
                    </mentions>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        let m = extract_explicit_mentions(&msg).expect("has mentions");
        assert!(m.mentions_everyone());
        assert!(m.has_broadcast());
        assert!(!m.mentions_here());
    }

    #[test]
    fn test_parse_jid_and_nick() {
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <mentions xmlns='urn:xmpp:emn:0'>\
                      <mention type='jid' value='alice@example.com'/>\
                      <mention type='nick' value='Bob'/>\
                    </mentions>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        let m = extract_explicit_mentions(&msg).expect("has mentions");
        assert!(m.mentions_jid("alice@example.com"));
        assert!(m.mentions_nick("Bob"));
        assert!(!m.has_broadcast());
    }

    #[test]
    fn test_extract_absent() {
        let msg = Message::new(None::<jid::Jid>);
        assert!(extract_explicit_mentions(&msg).is_none());
    }

    #[test]
    fn test_build_and_parse() {
        let mentions = ExplicitMentions::new()
            .with_everyone()
            .with_jid("alice@example.com")
            .with_nick("Bob");

        let elem = build_mentions_element(&mentions);
        let parsed = parse_mentions_element(&elem);

        assert_eq!(parsed.mentions.len(), 3);
        assert!(parsed.mentions_everyone());
        assert!(parsed.mentions_jid("alice@example.com"));
        assert!(parsed.mentions_nick("Bob"));
    }

    #[test]
    fn test_should_notify() {
        let m = ExplicitMentions::new().with_jid("alice@example.com");
        assert!(m.should_notify("alice@example.com", "alice"));
        assert!(!m.should_notify("bob@example.com", "bob"));

        let broadcast = ExplicitMentions::new().with_everyone();
        assert!(broadcast.should_notify("anyone@example.com", "anyone"));
    }

    #[test]
    fn test_set_explicit_mentions() {
        let mut msg = Message::new(None::<jid::Jid>);
        let mentions = ExplicitMentions::new().with_here();
        set_explicit_mentions(&mut msg, &mentions);

        assert!(has_explicit_mentions(&msg));

        // Replace
        let m2 = ExplicitMentions::new().with_everyone();
        set_explicit_mentions(&mut msg, &m2);
        let extracted = extract_explicit_mentions(&msg).expect("has mentions");
        assert!(extracted.mentions_everyone());
        assert!(!extracted.mentions_here());
    }

    #[test]
    fn test_strip_mentions() {
        let mut msg = Message::new(None::<jid::Jid>);
        set_explicit_mentions(&mut msg, &ExplicitMentions::new().with_everyone());
        strip_explicit_mentions(&mut msg);
        assert!(!has_explicit_mentions(&msg));
    }

    #[test]
    fn test_mention_type_helpers() {
        assert!(MentionType::Everyone.is_broadcast());
        assert!(MentionType::Here.is_broadcast());
        assert!(!MentionType::Jid("a".into()).is_broadcast());

        assert!(MentionType::Jid("a".into()).is_individual());
        assert!(MentionType::Nick("b".into()).is_individual());
        assert!(!MentionType::Everyone.is_individual());
    }

    #[test]
    fn test_role_mention() {
        let m = ExplicitMentions::new().with_mention(MentionType::Role("moderator".into()));
        assert!(!m.is_empty());
        assert_eq!(m.mentions[0].type_str(), "role");
        assert_eq!(m.mentions[0].value(), Some("moderator"));
    }

    #[test]
    fn test_explicit_mention_carrier_trait() {
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <mentions xmlns='urn:xmpp:emn:0'>\
                      <mention type='here'/>\
                    </mentions>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        assert!(msg.has_explicit_mentions());
        let m = msg.explicit_mentions().expect("has mentions");
        assert!(m.mentions_here());
    }

    #[test]
    fn test_empty_mentions() {
        let m = ExplicitMentions::new();
        assert!(m.is_empty());
        assert!(!m.has_broadcast());
        assert!(!m.should_notify("a@b", "a"));
    }
}

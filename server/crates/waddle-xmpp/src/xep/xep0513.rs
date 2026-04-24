//! XEP-0513: Explicit Mentions.

use jid::BareJid;
use minidom::Element;
use xmpp_parsers::message::Message;

/// Namespace for XEP-0513 Explicit Mentions.
pub const NS_EXPLICIT_MENTIONS: &str = "urn:xmpp:mentions:0";

/// XEP-0513 channel-wide mention URI.
pub const CHANNEL_MENTION: &str = "urn:xmpp:mentions:0#channel";

/// A single top-level `<mention/>` payload.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExplicitMention {
    pub begin: Option<u32>,
    pub end: Option<u32>,
    pub jid: Option<BareJid>,
    pub occupant_id: Option<String>,
    pub mentions: Option<String>,
    pub uri: Option<String>,
    pub active: bool,
    pub noping: bool,
}

impl ExplicitMention {
    pub fn jid(jid: BareJid) -> Self {
        Self {
            jid: Some(jid),
            ..Self::default()
        }
    }

    pub fn occupant_id(occupant_id: impl Into<String>) -> Self {
        Self {
            occupant_id: Some(occupant_id.into()),
            ..Self::default()
        }
    }

    pub fn channel() -> Self {
        Self {
            mentions: Some(CHANNEL_MENTION.to_string()),
            ..Self::default()
        }
    }

    pub fn active_channel() -> Self {
        Self {
            active: true,
            ..Self::channel()
        }
    }

    pub fn is_channel(&self) -> bool {
        self.mentions.as_deref() == Some(CHANNEL_MENTION)
    }

    pub fn is_individual(&self) -> bool {
        self.jid.is_some() || self.occupant_id.is_some()
    }
}

/// A set of explicit mentions in a message.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExplicitMentions {
    pub mentions: Vec<ExplicitMention>,
}

impl ExplicitMentions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_mention(mut self, mention: ExplicitMention) -> Self {
        self.mentions.push(mention);
        self
    }

    pub fn with_channel(self) -> Self {
        self.with_mention(ExplicitMention::channel())
    }

    pub fn with_active_channel(self) -> Self {
        self.with_mention(ExplicitMention::active_channel())
    }

    pub fn has_channel(&self) -> bool {
        self.mentions.iter().any(ExplicitMention::is_channel)
    }

    pub fn mentions_jid(&self, jid: &BareJid) -> bool {
        self.mentions.iter().any(|mention| {
            mention
                .jid
                .as_ref()
                .is_some_and(|mentioned| mentioned == jid)
        })
    }

    pub fn is_empty(&self) -> bool {
        self.mentions.is_empty()
    }
}

/// Trait for types that can carry explicit mentions.
pub trait ExplicitMentionCarrier {
    fn explicit_mentions(&self) -> Option<ExplicitMentions>;

    fn has_explicit_mentions(&self) -> bool {
        self.explicit_mentions().is_some_and(|m| !m.is_empty())
    }
}

impl ExplicitMentionCarrier for Message {
    fn explicit_mentions(&self) -> Option<ExplicitMentions> {
        extract_explicit_mentions(self)
    }
}

pub fn is_mention_element(elem: &Element) -> bool {
    elem.is("mention", NS_EXPLICIT_MENTIONS)
}

pub fn has_explicit_mentions(msg: &Message) -> bool {
    msg.payloads.iter().any(is_mention_element)
}

pub fn extract_explicit_mentions(msg: &Message) -> Option<ExplicitMentions> {
    let mentions: Vec<ExplicitMention> = msg
        .payloads
        .iter()
        .filter(|elem| is_mention_element(elem))
        .filter_map(parse_mention_element)
        .collect();

    if mentions.is_empty() {
        None
    } else {
        Some(ExplicitMentions { mentions })
    }
}

pub fn parse_mention_element(elem: &Element) -> Option<ExplicitMention> {
    let begin = elem.attr("begin").and_then(|value| value.parse().ok());
    let end = elem.attr("end").and_then(|value| value.parse().ok());
    let jid = elem.attr("jid").and_then(|value| value.parse().ok());
    let occupant_id = elem.attr("occupantid").map(str::to_string);
    let mentions = elem.attr("mentions").map(str::to_string);
    let uri = elem.attr("uri").map(str::to_string);
    let active = elem.get_child("active", NS_EXPLICIT_MENTIONS).is_some();
    let noping = elem.get_child("noping", NS_EXPLICIT_MENTIONS).is_some();

    if jid.is_none()
        && occupant_id.is_none()
        && mentions.is_none()
        && uri.is_none()
        && !active
        && !noping
    {
        return None;
    }

    Some(ExplicitMention {
        begin,
        end,
        jid,
        occupant_id,
        mentions,
        uri,
        active,
        noping,
    })
}

pub fn build_mention_element(mention: &ExplicitMention) -> Element {
    let mut elem = Element::builder("mention", NS_EXPLICIT_MENTIONS).build();

    if let Some(begin) = mention.begin {
        elem.set_attr("begin", begin.to_string());
    }
    if let Some(end) = mention.end {
        elem.set_attr("end", end.to_string());
    }
    if let Some(jid) = &mention.jid {
        elem.set_attr("jid", jid.to_string());
    }
    if let Some(occupant_id) = &mention.occupant_id {
        elem.set_attr("occupantid", occupant_id);
    }
    if let Some(mentions) = &mention.mentions {
        elem.set_attr("mentions", mentions);
    }
    if let Some(uri) = &mention.uri {
        elem.set_attr("uri", uri);
    }
    if mention.active {
        elem.append_child(Element::builder("active", NS_EXPLICIT_MENTIONS).build());
    }
    if mention.noping {
        elem.append_child(Element::builder("noping", NS_EXPLICIT_MENTIONS).build());
    }

    elem
}

pub fn build_mentions_elements(mentions: &ExplicitMentions) -> Vec<Element> {
    mentions
        .mentions
        .iter()
        .map(build_mention_element)
        .collect()
}

pub fn set_explicit_mentions(msg: &mut Message, mentions: &ExplicitMentions) {
    strip_explicit_mentions(msg);
    msg.payloads.extend(build_mentions_elements(mentions));
}

pub fn strip_explicit_mentions(msg: &mut Message) {
    msg.payloads
        .retain(|elem| elem.ns() != NS_EXPLICIT_MENTIONS);
}

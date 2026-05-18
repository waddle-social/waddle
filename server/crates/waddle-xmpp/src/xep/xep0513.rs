//! XEP-0513: Explicit Mentions.

use crate::types::Role;
use crate::xep::xep0004::{DataForm, Field, FieldOption, FieldType, FormType, IntoElement};
use jid::BareJid;
use minidom::Element;
use serde::{Deserialize, Serialize};
use xmpp_parsers::message::Message;

/// Namespace for XEP-0513 Explicit Mentions.
pub const NS_EXPLICIT_MENTIONS: &str = "urn:xmpp:mentions:0";

/// XEP-0513 channel-wide mention URI.
pub const CHANNEL_MENTION: &str = "urn:xmpp:mentions:0#channel";

pub const FIELD_MENTIONS_COUNT: &str = "mentions#count";
pub const FIELD_MENTIONS_INDIVIDUAL: &str = "mentions#individual";
pub const FIELD_MENTIONS_CHANNEL: &str = "mentions#channel";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MentionPermission {
    #[default]
    Participants,
    Moderators,
    None,
}

impl MentionPermission {
    pub fn as_form_value(self) -> &'static str {
        match self {
            Self::Participants => "participants",
            Self::Moderators => "moderators",
            Self::None => "none",
        }
    }

    pub fn from_form_value(value: &str) -> Option<Self> {
        match value {
            "participants" => Some(Self::Participants),
            "moderators" => Some(Self::Moderators),
            "none" => Some(Self::None),
            _ => None,
        }
    }

    pub fn allows_role(self, role: Role) -> bool {
        match self {
            Self::Participants => role >= Role::Participant,
            Self::Moderators => role >= Role::Moderator,
            Self::None => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MentionPermissions {
    pub count: u32,
    pub individual: MentionPermission,
    pub channel: MentionPermission,
}

impl Default for MentionPermissions {
    fn default() -> Self {
        Self {
            count: 5,
            individual: MentionPermission::Participants,
            channel: MentionPermission::Participants,
        }
    }
}

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

pub fn build_mentions_permissions_form(permissions: &MentionPermissions) -> Element {
    DataForm::new(FormType::Result)
        .add_field(Field::form_type(NS_EXPLICIT_MENTIONS))
        .add_field(
            Field::text_single(FIELD_MENTIONS_COUNT, permissions.count.to_string())
                .with_label("How many mentions are allowed in a message?")
                .with_required(),
        )
        .add_field(permission_field(
            FIELD_MENTIONS_INDIVIDUAL,
            "Who can mention individual users?",
            permissions.individual,
        ))
        .add_field(permission_field(
            FIELD_MENTIONS_CHANNEL,
            "Who can mention rooms?",
            permissions.channel,
        ))
        .into_element()
}

fn permission_field(var: &'static str, label: &'static str, value: MentionPermission) -> Field {
    Field::new(var, FieldType::ListSingle)
        .with_label(label)
        .with_required()
        .with_value(value.as_form_value())
        .add_option(FieldOption::with_label(
            "Participants",
            MentionPermission::Participants.as_form_value(),
        ))
        .add_option(FieldOption::with_label(
            "Moderators Only",
            MentionPermission::Moderators.as_form_value(),
        ))
        .add_option(FieldOption::with_label(
            "Nobody",
            MentionPermission::None.as_form_value(),
        ))
}

pub fn set_explicit_mentions(msg: &mut Message, mentions: &ExplicitMentions) {
    strip_explicit_mentions(msg);
    msg.payloads.extend(build_mentions_elements(mentions));
}

pub fn strip_explicit_mentions(msg: &mut Message) {
    msg.payloads
        .retain(|elem| elem.ns() != NS_EXPLICIT_MENTIONS);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xep::xep0004::NS_DATA_FORMS;

    #[test]
    fn xep0513_permission_values_match_role_thresholds() {
        assert!(MentionPermission::Participants.allows_role(Role::Participant));
        assert!(MentionPermission::Participants.allows_role(Role::Moderator));
        assert!(!MentionPermission::Participants.allows_role(Role::Visitor));
        assert!(MentionPermission::Moderators.allows_role(Role::Moderator));
        assert!(!MentionPermission::Moderators.allows_role(Role::Participant));
        assert!(!MentionPermission::None.allows_role(Role::Moderator));
    }

    #[test]
    fn xep0513_permissions_form_advertises_supported_subset() {
        let permissions = MentionPermissions {
            count: 3,
            individual: MentionPermission::Participants,
            channel: MentionPermission::Moderators,
        };
        let form = build_mentions_permissions_form(&permissions);
        assert_eq!(form.name(), "x");
        assert_eq!(form.ns(), NS_DATA_FORMS);
        assert_eq!(form.attr("type"), Some("result"));

        let form_type = form
            .children()
            .find(|child| child.attr("var") == Some("FORM_TYPE"))
            .and_then(|field| field.get_child("value", NS_DATA_FORMS))
            .map(|value| value.text())
            .expect("FORM_TYPE value");
        assert_eq!(form_type, NS_EXPLICIT_MENTIONS);

        let count = form
            .children()
            .find(|child| child.attr("var") == Some(FIELD_MENTIONS_COUNT))
            .and_then(|field| field.get_child("value", NS_DATA_FORMS))
            .map(|value| value.text())
            .expect("mentions#count value");
        assert_eq!(count, "3");

        let channel = form
            .children()
            .find(|child| child.attr("var") == Some(FIELD_MENTIONS_CHANNEL))
            .expect("mentions#channel field");
        assert_eq!(channel.attr("type"), Some("list-single"));
        assert_eq!(
            channel
                .get_child("value", NS_DATA_FORMS)
                .map(|value| value.text()),
            Some("moderators".to_string())
        );
    }
}

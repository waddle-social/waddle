//! Typed builders and parsers for MIX IQ payloads.
//!
//! All XML is constructed via `minidom::Element` per the project's
//! XML-generation rule (`CLAUDE.md`): no `format!`/string concatenation.

use jid::BareJid;
use minidom::Element;
use xmpp_parsers::iq::{Iq, IqType};

/// XEP-0369 core namespace.
pub const NS_MIX_CORE: &str = "urn:xmpp:mix:core:1";
/// XEP-0405 Participant Server Requirements.
pub const NS_MIX_PAM: &str = "urn:xmpp:mix:pam:2";
/// XEP-0407 Miscellaneous Capabilities.
pub const NS_MIX_MISC: &str = "urn:xmpp:mix:misc:0";

/// The subset of leaf nodes this server understands as routing targets.
///
/// Wire-level node names are the strings in [`MixLeaf::as_node_name`]; this
/// enum carries the same set for parser output.
pub use super::channel::MixLeaf as MixLeafNode;

/// Errors returned by MIX stanza parsers.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MixError {
    #[error("expected MIX element '{0}'")]
    ExpectedElement(&'static str),
    #[error("missing required attribute '{0}'")]
    MissingAttribute(&'static str),
    #[error("invalid leaf node name: {0}")]
    InvalidLeaf(String),
    #[error("IQ payload type is not IQ-set")]
    NotIqSet,
    #[error("no participant with nick '{0}'")]
    NoSuchNick(String),
}

/// A parsed `<join xmlns='urn:xmpp:mix:core:1'>` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinRequest {
    /// Optional nickname the caller wants to claim.
    pub nick: Option<String>,
    /// Leaf nodes the caller wants to subscribe to. Defaults to messages,
    /// participants, info if unspecified.
    pub subscribe: Vec<MixLeafNode>,
}

/// A parsed `<leave/>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaveRequest;

/// A parsed `<setnick nick='...'/>` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetnickRequest {
    pub nick: String,
}

/// A parsed `<update-subscription/>` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateSubscriptionRequest {
    pub subscribe: Vec<MixLeafNode>,
    pub unsubscribe: Vec<MixLeafNode>,
}

// --- Parsers ------------------------------------------------------------

fn iq_set_payload(iq: &Iq) -> Result<&Element, MixError> {
    match &iq.payload {
        IqType::Set(elem) => Ok(elem),
        _ => Err(MixError::NotIqSet),
    }
}

fn parse_subscribes(elem: &Element) -> Result<Vec<MixLeafNode>, MixError> {
    let mut out = Vec::new();
    for child in elem.children() {
        if child.is("subscribe", NS_MIX_CORE) {
            let node = child
                .attr("node")
                .ok_or(MixError::MissingAttribute("node"))?;
            let leaf = MixLeafNode::from_node_name(node)
                .ok_or_else(|| MixError::InvalidLeaf(node.to_string()))?;
            out.push(leaf);
        }
    }
    Ok(out)
}

/// Parse a `<join nick='…'><subscribe node='…'/>…</join>` payload from an
/// IQ-set whose payload is the `<join>` element itself.
pub fn parse_join(iq: &Iq) -> Result<JoinRequest, MixError> {
    let elem = iq_set_payload(iq)?;
    if !elem.is("join", NS_MIX_CORE) {
        return Err(MixError::ExpectedElement("join"));
    }

    let nick = elem
        .get_child("nick", NS_MIX_CORE)
        .map(|n| n.text())
        .filter(|s| !s.is_empty());
    let subscribe = parse_subscribes(elem)?;
    Ok(JoinRequest { nick, subscribe })
}

pub fn parse_leave(iq: &Iq) -> Result<LeaveRequest, MixError> {
    let elem = iq_set_payload(iq)?;
    if !elem.is("leave", NS_MIX_CORE) {
        return Err(MixError::ExpectedElement("leave"));
    }
    Ok(LeaveRequest)
}

pub fn parse_setnick(iq: &Iq) -> Result<SetnickRequest, MixError> {
    let elem = iq_set_payload(iq)?;
    if !elem.is("setnick", NS_MIX_CORE) {
        return Err(MixError::ExpectedElement("setnick"));
    }
    let nick = elem
        .get_child("nick", NS_MIX_CORE)
        .ok_or(MixError::MissingAttribute("nick"))?
        .text();
    if nick.is_empty() {
        return Err(MixError::MissingAttribute("nick"));
    }
    Ok(SetnickRequest { nick })
}

pub fn parse_update_subscription(iq: &Iq) -> Result<UpdateSubscriptionRequest, MixError> {
    let elem = iq_set_payload(iq)?;
    if !elem.is("update-subscription", NS_MIX_CORE) {
        return Err(MixError::ExpectedElement("update-subscription"));
    }
    let mut subscribe = Vec::new();
    let mut unsubscribe = Vec::new();
    for child in elem.children() {
        if child.is("subscribe", NS_MIX_CORE) {
            let node = child
                .attr("node")
                .ok_or(MixError::MissingAttribute("node"))?;
            let leaf = MixLeafNode::from_node_name(node)
                .ok_or_else(|| MixError::InvalidLeaf(node.to_string()))?;
            subscribe.push(leaf);
        } else if child.is("unsubscribe", NS_MIX_CORE) {
            let node = child
                .attr("node")
                .ok_or(MixError::MissingAttribute("node"))?;
            let leaf = MixLeafNode::from_node_name(node)
                .ok_or_else(|| MixError::InvalidLeaf(node.to_string()))?;
            unsubscribe.push(leaf);
        }
    }
    Ok(UpdateSubscriptionRequest {
        subscribe,
        unsubscribe,
    })
}

// --- Builders -----------------------------------------------------------

fn subscribe_elements(leaves: &[MixLeafNode]) -> Vec<Element> {
    leaves
        .iter()
        .map(|leaf| {
            Element::builder("subscribe", NS_MIX_CORE)
                .attr("node", leaf.as_node_name())
                .build()
        })
        .collect()
}

/// Build the `<join>` payload the server returns to confirm admission.
///
/// `participant_id` is the opaque participant id the server assigns (MIX's
/// notion of a stable per-channel identity). Callers typically use the same
/// identifier as occupant-id (XEP-0421) so clients can unify mentions.
pub fn build_join_result(
    original: &Iq,
    participant_id: &str,
    channel_jid: &BareJid,
    subscribed: &[MixLeafNode],
) -> Iq {
    let mut builder = Element::builder("join", NS_MIX_CORE)
        .attr("id", participant_id)
        .attr("jid", channel_jid.to_string());
    for sub in subscribe_elements(subscribed) {
        builder = builder.append(sub);
    }
    let elem = builder.build();
    Iq {
        from: Some(jid::Jid::from(channel_jid.clone())),
        to: original.from.clone(),
        id: original.id.clone(),
        payload: IqType::Result(Some(elem)),
    }
}

pub fn build_leave_result(original: &Iq, channel_jid: &BareJid) -> Iq {
    Iq {
        from: Some(jid::Jid::from(channel_jid.clone())),
        to: original.from.clone(),
        id: original.id.clone(),
        payload: IqType::Result(None),
    }
}

pub fn build_setnick_result(original: &Iq, channel_jid: &BareJid, nick: &str) -> Iq {
    let elem = Element::builder("setnick", NS_MIX_CORE)
        .append(Element::builder("nick", NS_MIX_CORE).append(nick).build())
        .build();
    Iq {
        from: Some(jid::Jid::from(channel_jid.clone())),
        to: original.from.clone(),
        id: original.id.clone(),
        payload: IqType::Result(Some(elem)),
    }
}

pub fn build_update_subscription_result(
    original: &Iq,
    channel_jid: &BareJid,
    subscribed: &[MixLeafNode],
) -> Iq {
    let mut builder = Element::builder("update-subscription", NS_MIX_CORE);
    for sub in subscribe_elements(subscribed) {
        builder = builder.append(sub);
    }
    let elem = builder.build();
    Iq {
        from: Some(jid::Jid::from(channel_jid.clone())),
        to: original.from.clone(),
        id: original.id.clone(),
        payload: IqType::Result(Some(elem)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mix::channel::MixLeaf;

    fn iq_set_with(child: Element) -> Iq {
        Iq {
            from: Some("alice@example.com/res".parse().unwrap()),
            to: Some("general@mix.example.com".parse().unwrap()),
            id: "iq-1".into(),
            payload: IqType::Set(child),
        }
    }

    #[test]
    fn test_parse_join_with_nick_and_subscribes() {
        let elem = Element::builder("join", NS_MIX_CORE)
            .append(
                Element::builder("nick", NS_MIX_CORE)
                    .append("Alice")
                    .build(),
            )
            .append(
                Element::builder("subscribe", NS_MIX_CORE)
                    .attr("node", MixLeaf::Messages.as_node_name())
                    .build(),
            )
            .append(
                Element::builder("subscribe", NS_MIX_CORE)
                    .attr("node", MixLeaf::Participants.as_node_name())
                    .build(),
            )
            .build();
        let iq = iq_set_with(elem);
        let parsed = parse_join(&iq).unwrap();
        assert_eq!(parsed.nick.as_deref(), Some("Alice"));
        assert_eq!(parsed.subscribe.len(), 2);
        assert!(parsed.subscribe.contains(&MixLeaf::Messages));
        assert!(parsed.subscribe.contains(&MixLeaf::Participants));
    }

    #[test]
    fn test_parse_join_rejects_unknown_leaf() {
        let elem = Element::builder("join", NS_MIX_CORE)
            .append(
                Element::builder("subscribe", NS_MIX_CORE)
                    .attr("node", "urn:xmpp:mix:nodes:bogus")
                    .build(),
            )
            .build();
        let iq = iq_set_with(elem);
        assert!(matches!(parse_join(&iq), Err(MixError::InvalidLeaf(_))));
    }

    #[test]
    fn test_parse_leave() {
        let iq = iq_set_with(Element::builder("leave", NS_MIX_CORE).build());
        assert_eq!(parse_leave(&iq).unwrap(), LeaveRequest);
    }

    #[test]
    fn test_parse_setnick() {
        let iq = iq_set_with(
            Element::builder("setnick", NS_MIX_CORE)
                .append(Element::builder("nick", NS_MIX_CORE).append("Ally").build())
                .build(),
        );
        assert_eq!(parse_setnick(&iq).unwrap().nick, "Ally");
    }

    #[test]
    fn test_parse_update_subscription() {
        let iq = iq_set_with(
            Element::builder("update-subscription", NS_MIX_CORE)
                .append(
                    Element::builder("subscribe", NS_MIX_CORE)
                        .attr("node", MixLeaf::Config.as_node_name())
                        .build(),
                )
                .append(
                    Element::builder("unsubscribe", NS_MIX_CORE)
                        .attr("node", MixLeaf::Info.as_node_name())
                        .build(),
                )
                .build(),
        );
        let parsed = parse_update_subscription(&iq).unwrap();
        assert_eq!(parsed.subscribe, vec![MixLeaf::Config]);
        assert_eq!(parsed.unsubscribe, vec![MixLeaf::Info]);
    }

    #[test]
    fn test_build_join_result_round_trip() {
        let elem = Element::builder("join", NS_MIX_CORE).build();
        let iq = iq_set_with(elem);
        let channel: BareJid = "general@mix.example.com".parse().unwrap();
        let out = build_join_result(
            &iq,
            "participant-123",
            &channel,
            &[MixLeaf::Messages, MixLeaf::Participants],
        );
        let payload = match out.payload {
            IqType::Result(Some(ref e)) => e,
            _ => panic!("expected IQ result"),
        };
        assert_eq!(payload.attr("id"), Some("participant-123"));
        assert_eq!(payload.attr("jid"), Some(channel.to_string()).as_deref());
        assert_eq!(payload.children().count(), 2);
    }

    #[test]
    fn test_parse_leave_wrong_element() {
        let iq = iq_set_with(Element::builder("join", NS_MIX_CORE).build());
        assert!(matches!(
            parse_leave(&iq),
            Err(MixError::ExpectedElement("leave"))
        ));
    }

    #[test]
    fn test_parse_not_iq_set() {
        let iq = Iq {
            from: None,
            to: None,
            id: "x".into(),
            payload: IqType::Get(Element::builder("join", NS_MIX_CORE).build()),
        };
        assert_eq!(parse_join(&iq), Err(MixError::NotIqSet));
    }
}

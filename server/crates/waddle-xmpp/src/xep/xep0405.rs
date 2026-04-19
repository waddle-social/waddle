//! XEP-0405: MIX — Participant Server Requirements (MIX-PAM).
//!
//! Implements the "client joins via my own server" wrapper flow: the client
//! sends an IQ to *its* own server with a `<client-join>` element whose child
//! is the real MIX-core `<join>` payload destined for the channel. The
//! server then talks to the MIX channel on the client's behalf, persists the
//! participant record (see `mix_subscriptions` in the database) and returns
//! a `<client-join/>` result.
//!
//! Symmetric flows exist for `<client-leave/>`.

use jid::BareJid;
use minidom::Element;
use xmpp_parsers::iq::{Iq, IqType};

pub use crate::mix::pam::{MixRoster, MixSubscription};
use crate::mix::NS_MIX_CORE;
pub use crate::mix::NS_MIX_PAM;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PamError {
    #[error("expected element '{0}' in MIX-PAM namespace")]
    ExpectedElement(&'static str),
    #[error("missing required 'channel' attribute")]
    MissingChannel,
    #[error("invalid channel JID: {0}")]
    InvalidChannelJid(String),
    #[error("inner MIX-core payload is missing or not in the core namespace")]
    MissingCorePayload,
    #[error("IQ is not an IQ-set")]
    NotIqSet,
}

/// Parsed `<client-join>` IQ wrapper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientJoin {
    pub channel: BareJid,
    /// The inner `<join xmlns='urn:xmpp:mix:core:1'>` element verbatim —
    /// the server forwards this to the target channel as-is.
    pub inner_join: Element,
}

/// Parsed `<client-leave>` IQ wrapper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientLeave {
    pub channel: BareJid,
}

fn payload_set(iq: &Iq) -> Result<&Element, PamError> {
    match &iq.payload {
        IqType::Set(e) => Ok(e),
        _ => Err(PamError::NotIqSet),
    }
}

pub fn parse_client_join(iq: &Iq) -> Result<ClientJoin, PamError> {
    let elem = payload_set(iq)?;
    if !elem.is("client-join", NS_MIX_PAM) {
        return Err(PamError::ExpectedElement("client-join"));
    }
    let channel_attr = elem.attr("channel").ok_or(PamError::MissingChannel)?;
    let channel: BareJid = channel_attr
        .parse()
        .map_err(|_| PamError::InvalidChannelJid(channel_attr.to_string()))?;
    let inner_join = elem
        .get_child("join", NS_MIX_CORE)
        .ok_or(PamError::MissingCorePayload)?
        .clone();
    Ok(ClientJoin {
        channel,
        inner_join,
    })
}

pub fn parse_client_leave(iq: &Iq) -> Result<ClientLeave, PamError> {
    let elem = payload_set(iq)?;
    if !elem.is("client-leave", NS_MIX_PAM) {
        return Err(PamError::ExpectedElement("client-leave"));
    }
    let channel_attr = elem.attr("channel").ok_or(PamError::MissingChannel)?;
    let channel: BareJid = channel_attr
        .parse()
        .map_err(|_| PamError::InvalidChannelJid(channel_attr.to_string()))?;
    Ok(ClientLeave { channel })
}

/// Build the `<client-join>` result that the user's own server returns.
pub fn build_client_join_result(original: &Iq, channel: &BareJid, joined: Element) -> Iq {
    let elem = Element::builder("client-join", NS_MIX_PAM)
        .attr("channel", channel.to_string())
        .append(joined)
        .build();
    Iq {
        from: original.to.clone(),
        to: original.from.clone(),
        id: original.id.clone(),
        payload: IqType::Result(Some(elem)),
    }
}

pub fn build_client_leave_result(original: &Iq, channel: &BareJid) -> Iq {
    let elem = Element::builder("client-leave", NS_MIX_PAM)
        .attr("channel", channel.to_string())
        .build();
    Iq {
        from: original.to.clone(),
        to: original.from.clone(),
        id: original.id.clone(),
        payload: IqType::Result(Some(elem)),
    }
}

pub fn is_mix_pam_iq(iq: &Iq) -> bool {
    let elem = match &iq.payload {
        IqType::Set(e) | IqType::Get(e) => e,
        _ => return false,
    };
    if elem.ns() != NS_MIX_PAM {
        return false;
    }
    matches!(elem.name(), "client-join" | "client-leave")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iq_set(child: Element) -> Iq {
        Iq {
            from: Some("alice@example.com/res".parse().unwrap()),
            to: Some("alice@example.com".parse().unwrap()),
            id: "iq-7".into(),
            payload: IqType::Set(child),
        }
    }

    #[test]
    fn test_parse_client_join_success() {
        let inner = Element::builder("join", NS_MIX_CORE)
            .append(
                Element::builder("nick", NS_MIX_CORE)
                    .append("Alice")
                    .build(),
            )
            .build();
        let wrapper = Element::builder("client-join", NS_MIX_PAM)
            .attr("channel", "general@mix.example.com")
            .append(inner.clone())
            .build();
        let parsed = parse_client_join(&iq_set(wrapper)).unwrap();
        assert_eq!(parsed.channel.to_string(), "general@mix.example.com");
        assert_eq!(parsed.inner_join, inner);
    }

    #[test]
    fn test_parse_client_join_missing_channel() {
        let inner = Element::builder("join", NS_MIX_CORE).build();
        let wrapper = Element::builder("client-join", NS_MIX_PAM)
            .append(inner)
            .build();
        assert_eq!(
            parse_client_join(&iq_set(wrapper)),
            Err(PamError::MissingChannel)
        );
    }

    #[test]
    fn test_parse_client_join_missing_inner() {
        let wrapper = Element::builder("client-join", NS_MIX_PAM)
            .attr("channel", "g@mix.example.com")
            .build();
        assert_eq!(
            parse_client_join(&iq_set(wrapper)),
            Err(PamError::MissingCorePayload)
        );
    }

    #[test]
    fn test_parse_client_leave() {
        let elem = Element::builder("client-leave", NS_MIX_PAM)
            .attr("channel", "g@mix.example.com")
            .build();
        let parsed = parse_client_leave(&iq_set(elem)).unwrap();
        assert_eq!(parsed.channel.to_string(), "g@mix.example.com");
    }

    #[test]
    fn test_is_mix_pam_iq() {
        let iq = iq_set(
            Element::builder("client-join", NS_MIX_PAM)
                .attr("channel", "g@mix.example.com")
                .append(Element::builder("join", NS_MIX_CORE).build())
                .build(),
        );
        assert!(is_mix_pam_iq(&iq));
    }

    #[test]
    fn test_build_client_join_result() {
        let inner = Element::builder("join", NS_MIX_CORE).build();
        let wrapper = Element::builder("client-join", NS_MIX_PAM)
            .attr("channel", "general@mix.example.com")
            .append(inner.clone())
            .build();
        let iq = iq_set(wrapper);
        let channel: BareJid = "general@mix.example.com".parse().unwrap();
        let out = build_client_join_result(&iq, &channel, inner);
        match out.payload {
            IqType::Result(Some(e)) => {
                assert!(e.is("client-join", NS_MIX_PAM));
                assert_eq!(e.attr("channel"), Some("general@mix.example.com"));
                assert!(e.get_child("join", NS_MIX_CORE).is_some());
            }
            _ => panic!("expected result payload"),
        }
    }
}

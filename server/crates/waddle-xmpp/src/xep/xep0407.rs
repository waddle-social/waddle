//! XEP-0407: Mediated Information eXchange (MIX) — Miscellaneous Capabilities.
//!
//! Covers the small ancillary requests that orbit the MIX core:
//! `<invite>`/`<invitation>` for out-of-band-free invite handoff, the
//! `<register-nick/>` sub-request, and the `<setnick/>` extended form.
//!
//! The disco feature name for this set is [`NS_MIX_MISC`].

use jid::BareJid;
use minidom::Element;
use xmpp_parsers::iq::{Iq, IqType};

use crate::mix::NS_MIX_CORE;
pub use crate::mix::NS_MIX_MISC;

/// Errors returned by MIX-misc parsers.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MiscError {
    #[error("expected element '{0}' in MIX-misc namespace")]
    ExpectedElement(&'static str),
    #[error("missing required attribute '{0}'")]
    MissingAttribute(&'static str),
    #[error("invalid JID '{0}'")]
    InvalidJid(String),
    #[error("IQ payload is not an IQ-set")]
    NotIqSet,
}

/// A parsed `<invite>` IQ — A asks the channel to issue an invitation that
/// can be forwarded to B out of band.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteRequest {
    pub invitee: BareJid,
}

/// A parsed `<invitation>` element — the token that lets B join the channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invitation {
    pub inviter: BareJid,
    pub invitee: BareJid,
    pub channel: BareJid,
    pub token: String,
}

fn iq_set_payload(iq: &Iq) -> Result<&Element, MiscError> {
    match &iq.payload {
        IqType::Set(e) => Ok(e),
        _ => Err(MiscError::NotIqSet),
    }
}

fn parse_bare_jid(raw: &str) -> Result<BareJid, MiscError> {
    raw.parse().map_err(|_| MiscError::InvalidJid(raw.into()))
}

pub fn parse_invite_request(iq: &Iq) -> Result<InviteRequest, MiscError> {
    let elem = iq_set_payload(iq)?;
    if !elem.is("invite", NS_MIX_MISC) {
        return Err(MiscError::ExpectedElement("invite"));
    }
    let invitee_elem = elem
        .get_child("invitee", NS_MIX_MISC)
        .ok_or(MiscError::MissingAttribute("invitee"))?;
    let invitee = parse_bare_jid(invitee_elem.text().trim())?;
    Ok(InviteRequest { invitee })
}

pub fn parse_invitation(elem: &Element) -> Result<Invitation, MiscError> {
    if !elem.is("invitation", NS_MIX_MISC) {
        return Err(MiscError::ExpectedElement("invitation"));
    }
    let inviter_raw = elem
        .get_child("inviter", NS_MIX_MISC)
        .ok_or(MiscError::MissingAttribute("inviter"))?
        .text();
    let invitee_raw = elem
        .get_child("invitee", NS_MIX_MISC)
        .ok_or(MiscError::MissingAttribute("invitee"))?
        .text();
    let channel_raw = elem
        .get_child("channel", NS_MIX_MISC)
        .ok_or(MiscError::MissingAttribute("channel"))?
        .text();
    let token = elem
        .get_child("token", NS_MIX_MISC)
        .ok_or(MiscError::MissingAttribute("token"))?
        .text();
    if token.is_empty() {
        return Err(MiscError::MissingAttribute("token"));
    }
    Ok(Invitation {
        inviter: parse_bare_jid(inviter_raw.trim())?,
        invitee: parse_bare_jid(invitee_raw.trim())?,
        channel: parse_bare_jid(channel_raw.trim())?,
        token,
    })
}

/// Build the `<invitation>` payload returned on a successful `<invite>`.
pub fn build_invitation_element(invitation: &Invitation) -> Element {
    Element::builder("invitation", NS_MIX_MISC)
        .append(
            Element::builder("inviter", NS_MIX_MISC)
                .append(invitation.inviter.to_string())
                .build(),
        )
        .append(
            Element::builder("invitee", NS_MIX_MISC)
                .append(invitation.invitee.to_string())
                .build(),
        )
        .append(
            Element::builder("channel", NS_MIX_MISC)
                .append(invitation.channel.to_string())
                .build(),
        )
        .append(
            Element::builder("token", NS_MIX_MISC)
                .append(invitation.token.as_str())
                .build(),
        )
        .build()
}

pub fn build_invite_result(original: &Iq, invitation: &Invitation) -> Iq {
    Iq {
        from: original.to.clone(),
        to: original.from.clone(),
        id: original.id.clone(),
        payload: IqType::Result(Some(build_invitation_element(invitation))),
    }
}

/// Attach an `<invitation>` element to a message so the invitee's client can
/// render it. This is the forwarding form that travels between users.
pub fn set_invitation_on_message(
    msg: &mut xmpp_parsers::message::Message,
    invitation: &Invitation,
) {
    msg.payloads.push(build_invitation_element(invitation));
}

/// True when an IQ carries one of the `NS_MIX_MISC` requests this module
/// parses.
pub fn is_mix_misc_iq(iq: &Iq) -> bool {
    let elem = match &iq.payload {
        IqType::Set(e) | IqType::Get(e) => e,
        _ => return false,
    };
    if elem.ns() != NS_MIX_MISC {
        return false;
    }
    matches!(elem.name(), "invite" | "invitation")
}

/// The feature strings advertised on `mix.<domain>` service discovery.
pub fn mix_disco_features() -> &'static [&'static str] {
    &[NS_MIX_CORE, crate::mix::NS_MIX_PAM, NS_MIX_MISC]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iq_set(child: Element) -> Iq {
        Iq {
            from: Some("alice@example.com/res".parse().unwrap()),
            to: Some("general@mix.example.com".parse().unwrap()),
            id: "iq-misc-1".into(),
            payload: IqType::Set(child),
        }
    }

    fn sample_invitation() -> Invitation {
        Invitation {
            inviter: "alice@example.com".parse().unwrap(),
            invitee: "bob@example.com".parse().unwrap(),
            channel: "general@mix.example.com".parse().unwrap(),
            token: "tok-xyz".into(),
        }
    }

    #[test]
    fn test_parse_invite_request() {
        let elem = Element::builder("invite", NS_MIX_MISC)
            .append(
                Element::builder("invitee", NS_MIX_MISC)
                    .append("bob@example.com")
                    .build(),
            )
            .build();
        let parsed = parse_invite_request(&iq_set(elem)).unwrap();
        assert_eq!(parsed.invitee.to_string(), "bob@example.com");
    }

    #[test]
    fn test_parse_invite_missing_invitee() {
        let elem = Element::builder("invite", NS_MIX_MISC).build();
        assert_eq!(
            parse_invite_request(&iq_set(elem)),
            Err(MiscError::MissingAttribute("invitee"))
        );
    }

    #[test]
    fn test_invitation_round_trip() {
        let inv = sample_invitation();
        let elem = build_invitation_element(&inv);
        let parsed = parse_invitation(&elem).unwrap();
        assert_eq!(parsed, inv);
    }

    #[test]
    fn test_parse_invitation_rejects_empty_token() {
        let elem = Element::builder("invitation", NS_MIX_MISC)
            .append(
                Element::builder("inviter", NS_MIX_MISC)
                    .append("alice@example.com")
                    .build(),
            )
            .append(
                Element::builder("invitee", NS_MIX_MISC)
                    .append("bob@example.com")
                    .build(),
            )
            .append(
                Element::builder("channel", NS_MIX_MISC)
                    .append("general@mix.example.com")
                    .build(),
            )
            .append(Element::builder("token", NS_MIX_MISC).build())
            .build();
        assert_eq!(
            parse_invitation(&elem),
            Err(MiscError::MissingAttribute("token"))
        );
    }

    #[test]
    fn test_build_invite_result_shape() {
        let iq = iq_set(
            Element::builder("invite", NS_MIX_MISC)
                .append(
                    Element::builder("invitee", NS_MIX_MISC)
                        .append("bob@example.com")
                        .build(),
                )
                .build(),
        );
        let out = build_invite_result(&iq, &sample_invitation());
        match out.payload {
            IqType::Result(Some(e)) => {
                assert!(e.is("invitation", NS_MIX_MISC));
                assert!(e.get_child("token", NS_MIX_MISC).is_some());
            }
            _ => panic!("expected result payload"),
        }
    }

    #[test]
    fn test_is_mix_misc_iq() {
        assert!(is_mix_misc_iq(&iq_set(
            Element::builder("invite", NS_MIX_MISC).build()
        )));
        assert!(is_mix_misc_iq(&iq_set(
            Element::builder("invitation", NS_MIX_MISC).build()
        )));
        assert!(!is_mix_misc_iq(&iq_set(
            Element::builder("invite", "other").build()
        )));
    }

    #[test]
    fn test_set_invitation_on_message() {
        use xmpp_parsers::message::Message;
        let mut msg = Message::new(Some(jid::Jid::from(
            "bob@example.com".parse::<BareJid>().unwrap(),
        )));
        set_invitation_on_message(&mut msg, &sample_invitation());
        assert!(msg.payloads.iter().any(|p| p.is("invitation", NS_MIX_MISC)));
    }

    #[test]
    fn test_mix_disco_features_contains_three_namespaces() {
        let feats = mix_disco_features();
        assert_eq!(feats.len(), 3);
        assert!(feats.contains(&NS_MIX_CORE));
        assert!(feats.contains(&crate::mix::NS_MIX_PAM));
        assert!(feats.contains(&NS_MIX_MISC));
    }
}

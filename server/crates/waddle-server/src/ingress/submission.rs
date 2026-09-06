//! Immutable planning input reused by every transaction attempt.
use super::identity::IngressStreamIdentity;
use crate::server::routes::interpret::effects::IngressPlan;
use jid::{BareJid, DomainRef};
use waddle_xmpp::{
    auth::AuthenticatedPrincipalRef,
    ingress::{ConnectionGeneration, DigestInput, NormalizedTarget},
};
use xmpp_parsers::message::{Message, MessageType};

#[derive(Clone, Debug)]
pub struct IngressSubmission {
    pub identity: IngressStreamIdentity,
    pub principal: AuthenticatedPrincipalRef,
    /// Authenticated sender before room canonicalization changes the envelope address.
    pub sender: jid::FullJid,
    pub target: NormalizedTarget,
    pub plan: IngressPlan,
    pub digest_input: DigestInput,
    pub connection_generation: ConnectionGeneration,
}

/// Normalize client-supplied XEP-0359 stamps at the admission binding before
/// computing the pre-enrichment digest. Planning retains its own stamp handling.
pub fn digest_input(
    message: &Message,
    context: &waddle_xmpp::ingress::DigestContext,
) -> Result<DigestInput, waddle_xmpp::ingress::DigestInputError> {
    let normalized = message_for_digest(message, &context.server_authorities);
    DigestInput::from_parsed(&normalized, context)
}

fn message_for_digest(message: &Message, authorities: &[BareJid]) -> Message {
    let mut normalized = message.clone();
    for authority in authorities {
        waddle_xmpp_core::xep0359::remove_stanza_ids_by(&mut normalized, &authority.clone().into());
    }
    normalized
}

/// Determine the assigning room authority from message shape before handlers run.
/// Covers XEP-0045 groupchat, occupant PM, mediated invitation and decline forms.
pub fn room_scope(message: &Message, muc_domain: &DomainRef) -> Option<BareJid> {
    let to = message.to.as_ref()?;
    let room = to.to_bare();
    if room.domain().as_str() != muc_domain.as_str() {
        return None;
    }
    if to.resource().is_some() || message.type_ == MessageType::Groupchat {
        return Some(room);
    }
    let namespace = waddle_xmpp::muc::presence::NS_MUC_USER;
    message
        .payloads
        .iter()
        .find(|payload| payload.is("x", namespace))
        .and_then(|payload| {
            (payload.get_child("invite", namespace).is_some()
                || payload.get_child("decline", namespace).is_some())
            .then_some(room)
        })
}

/// Digest authorities are assigned from the offered stanza, never from a later handler outcome.
pub fn digest_authorities(
    message: &Message,
    sender: &BareJid,
    muc_domain: &DomainRef,
) -> Vec<BareJid> {
    let mut authorities = vec![sender.clone()];
    if let Some(room) = room_scope(message, muc_domain) {
        if room != *sender {
            authorities.push(room);
        }
    }
    authorities
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn room_scope_classifies_all_muc_message_shapes() {
        let domain: BareJid = "muc.example.test".parse().expect("domain");
        let room: BareJid = "room@muc.example.test".parse().expect("room");
        for (target, kind, child, expected) in [
            ("room@muc.example.test", MessageType::Groupchat, None, true),
            ("room@muc.example.test/nick", MessageType::Chat, None, true),
            (
                "room@muc.example.test",
                MessageType::Normal,
                Some("invite"),
                true,
            ),
            (
                "room@muc.example.test",
                MessageType::Normal,
                Some("decline"),
                true,
            ),
            ("room@muc.example.test", MessageType::Chat, None, false),
            ("user@example.test", MessageType::Groupchat, None, false),
        ] {
            let mut message = Message::new(Some(target.parse().expect("target")));
            message.type_ = kind;
            if let Some(child) = child {
                let ns = waddle_xmpp::muc::presence::NS_MUC_USER;
                message.payloads.push(
                    minidom::Element::builder("x", ns)
                        .append(minidom::Element::builder(child, ns).build())
                        .build(),
                );
            }
            assert_eq!(
                room_scope(&message, domain.domain()),
                expected.then_some(room.clone())
            );
        }
    }

    #[test]
    fn digest_authorities_include_sender_and_shape_room() {
        let sender: BareJid = "sender@example.test".parse().expect("sender");
        let room: BareJid = "room@muc.example.test".parse().expect("room");
        let mut message = Message::new(Some(room.clone().into()));
        message.type_ = MessageType::Groupchat;
        assert_eq!(
            digest_authorities(&message, &sender, room.domain()),
            vec![sender.clone(), room.clone()]
        );
        assert_eq!(
            digest_authorities(&message, &room, room.domain()),
            vec![room]
        );
        message.to = None;
        assert_eq!(
            digest_authorities(&message, &sender, sender.domain()),
            vec![sender]
        );
    }
}

#[cfg(test)]
mod digest_binding_tests {
    use super::*;
    use waddle_xmpp::ingress::DigestContext;
    use waddle_xmpp_core::xep0359::{add_stanza_id, StanzaId, NS_SID};

    #[test]
    fn digest_binding_strips_only_trusted_stamps_without_mutating_the_offered_message() {
        let sender: BareJid = "alice@example.com".parse().expect("sender");
        let foreign: BareJid = "other.example.net".parse().expect("foreign authority");
        let mut offered = Message::new(None);
        add_stanza_id(
            &mut offered,
            &StanzaId::new("forged", sender.clone().into()),
        );
        add_stanza_id(
            &mut offered,
            &StanzaId::new("foreign", foreign.clone().into()),
        );
        let context = DigestContext {
            target: NormalizedTarget::Absent,
            server_authorities: vec![sender],
            stanza_lang: None,
        };
        assert!(matches!(
            DigestInput::from_parsed(&offered, &context),
            Err(waddle_xmpp::ingress::DigestInputError::ForgedServerStanzaId { .. })
        ));
        let normalized = message_for_digest(&offered, &context.server_authorities);
        assert_eq!(normalized.payloads.len(), 1);
        assert!(normalized.payloads[0].is("stanza-id", NS_SID));
        assert_eq!(
            normalized.payloads[0].attr("by"),
            Some(foreign.to_string().as_str())
        );
        assert_eq!(offered.payloads.len(), 2);
        digest_input(&offered, &context).expect("normalization permits admission");
    }
}

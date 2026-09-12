use super::*;
use crate::ingress_substrate::MessageEnvelope;
use waddle_xmpp_core::xep0359::{add_stanza_id, extract_stanza_id_by, StanzaId};
use xmpp_parsers::message::{Message, MessageType};

#[test]
fn delivery_copy_restores_canonical_content_address_and_recipient_stamp() {
    let recipient: jid::BareJid = "juliet@example.com".parse().expect("recipient");
    let sender: jid::BareJid = "romeo@example.com".parse().expect("sender");
    let saved = StanzaId::new("recipient-saved", recipient.clone().into());
    let intent = IngressEffectIntent::ArchiveAuthoritative {
        ordinal: None,
        archive: recipient.clone(),
        by: recipient.clone(),
        stanza_id: saved.clone(),
        archived_at: chrono::DateTime::from_timestamp(100, 0).expect("timestamp"),
    };
    for target in [
        recipient.clone().into(),
        "juliet@example.com/phone".parse().expect("full"),
    ] {
        let mut canonical = Message::new(Some(target));
        canonical.type_ = MessageType::Chat;
        canonical.from = Some(sender.clone().into());
        canonical
            .bodies
            .insert(Default::default(), "canonical".into());
        add_stanza_id(
            &mut canonical,
            &StanzaId::new("sender-saved", sender.clone().into()),
        );
        add_stanza_id(
            &mut canonical,
            &StanzaId::new("recipient-old", recipient.clone().into()),
        );
        let envelope = MessageEnvelope::new(canonical.clone());
        let copy = delivery_message(&envelope, &recipient, std::slice::from_ref(&intent));
        assert_eq!(copy.to, canonical.to);
        assert_eq!(copy.from, canonical.from);
        assert_eq!(copy.bodies, canonical.bodies);
        assert_eq!(
            extract_stanza_id_by(&copy, &recipient.clone().into()),
            Some(saved.id.clone())
        );
        assert_eq!(
            extract_stanza_id_by(&copy, &sender.clone().into()),
            Some("sender-saved".into())
        );
        assert_eq!(waddle_xmpp::xep::extract_stanza_ids(&copy).len(), 2);
    }
}

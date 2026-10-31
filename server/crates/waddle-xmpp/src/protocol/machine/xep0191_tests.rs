//! XEP-0191 §3.3 and note 8: IQ blocking respects account identity.

use minidom::Element;
use xmpp_parsers::iq::{Iq, IqHeader, IqPayload};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use super::test_support::ready_machine;
use super::{Blocklist, InboundEvent, OutboundEvent, Stanza, StanzaDispatcher};
use crate::xep::xep0199::NS_PING;

#[test]
fn own_resources_exchange_all_iq_types_despite_matching_block_rules() {
    assert_iq_block_policy("alice@example.com/web", true);
    assert_iq_block_policy("alice@example.com/phone", true);
}

#[test]
fn different_and_foreign_accounts_remain_blocked_for_all_iq_types() {
    assert_iq_block_policy("bob@example.com/web", false);
    assert_iq_block_policy("alice@foreign.test/web", false);
}

fn assert_iq_block_policy(sender: &str, delivered: bool) {
    let recipient = "alice@example.com/phone".parse().expect("recipient");
    let mut machine = ready_machine("example.com", recipient, StanzaDispatcher::new());
    machine.set_blocklist(Blocklist::new([
        "example.com".parse::<jid::Jid>().expect("domain block"),
        "foreign.test".parse::<jid::Jid>().expect("foreign block"),
    ]));
    let payload = Element::builder("ping", NS_PING).build();
    for data in [
        IqPayload::Get(payload.clone()),
        IqPayload::Set(payload),
        IqPayload::Result(None),
        IqPayload::Error(StanzaError::new(
            ErrorType::Cancel,
            DefinedCondition::ServiceUnavailable,
            "en",
            "Unavailable.",
        )),
    ] {
        let iq: Iq = IqHeader {
            from: Some(sender.parse().expect("sender")),
            to: Some("alice@example.com/phone".parse().expect("target")),
            id: "blocking-policy".into(),
        }
        .assemble(data);
        let events = machine.handle(InboundEvent::StanzaFromPeer(Box::new(Stanza::Iq(
            Box::new(iq.clone()),
        ))));
        let sent: Vec<_> = events
            .into_iter()
            .filter_map(|event| match event {
                OutboundEvent::SendStanza(stanza) => Some(*stanza),
                _ => None,
            })
            .collect();
        if delivered {
            assert!(matches!(sent.as_slice(), [Stanza::Iq(actual)] if actual.as_ref() == &iq));
        } else {
            assert!(
                sent.is_empty(),
                "blocked peer must not reach the wire or receive an error loop"
            );
        }
    }
}

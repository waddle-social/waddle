//! XEP-0405: MIX Participant Server Requirements (MIX-PAM) — dedicated suite.

use minidom::Element;
use waddle_xmpp::mix::pam::{MixRoster, MixSubscription};
use waddle_xmpp::mix::{NS_MIX_CORE, NS_MIX_PAM};
use waddle_xmpp::xep::xep0405::{
    build_client_join_result, build_client_leave_result, is_mix_pam_iq, parse_client_join,
    parse_client_leave, PamError,
};
use xmpp_parsers::iq::{Iq, IqType};

fn iq_set(child: Element) -> Iq {
    Iq {
        from: Some("alice@example.com/r".parse().unwrap()),
        to: Some("alice@example.com".parse().unwrap()),
        id: "iq-pam-1".into(),
        payload: IqType::Set(child),
    }
}

#[test]
fn pam_client_join_round_trip() {
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
    let iq = iq_set(wrapper);
    let parsed = parse_client_join(&iq).unwrap();
    assert_eq!(parsed.channel.to_string(), "general@mix.example.com");
    assert_eq!(parsed.inner_join, inner);

    let result = build_client_join_result(&iq, &parsed.channel, inner);
    match result.payload {
        IqType::Result(Some(e)) => {
            assert!(e.is("client-join", NS_MIX_PAM));
            assert_eq!(e.attr("channel"), Some("general@mix.example.com"));
            assert!(e.get_child("join", NS_MIX_CORE).is_some());
        }
        _ => panic!("expected result"),
    }
}

#[test]
fn pam_client_leave_round_trip() {
    let elem = Element::builder("client-leave", NS_MIX_PAM)
        .attr("channel", "general@mix.example.com")
        .build();
    let iq = iq_set(elem);
    let parsed = parse_client_leave(&iq).unwrap();
    assert_eq!(parsed.channel.to_string(), "general@mix.example.com");
    let result = build_client_leave_result(&iq, &parsed.channel);
    matches!(result.payload, IqType::Result(Some(_)));
}

#[test]
fn pam_errors_missing_attribute_and_child() {
    let elem = Element::builder("client-join", NS_MIX_PAM)
        .append(Element::builder("join", NS_MIX_CORE).build())
        .build();
    assert_eq!(
        parse_client_join(&iq_set(elem)),
        Err(PamError::MissingChannel)
    );

    let elem = Element::builder("client-join", NS_MIX_PAM)
        .attr("channel", "g@mix.example.com")
        .build();
    assert_eq!(
        parse_client_join(&iq_set(elem)),
        Err(PamError::MissingCorePayload)
    );
}

#[test]
fn pam_invalid_channel_jid_rejected() {
    // A string with whitespace inside the localpart is not a valid bare JID.
    let elem = Element::builder("client-leave", NS_MIX_PAM)
        .attr("channel", "@@@@")
        .build();
    assert!(matches!(
        parse_client_leave(&iq_set(elem)),
        Err(PamError::InvalidChannelJid(_))
    ));
}

#[test]
fn pam_is_mix_pam_iq() {
    let iq = iq_set(
        Element::builder("client-join", NS_MIX_PAM)
            .attr("channel", "g@mix.example.com")
            .append(Element::builder("join", NS_MIX_CORE).build())
            .build(),
    );
    assert!(is_mix_pam_iq(&iq));

    let iq2 = iq_set(Element::builder("client-join", "other").build());
    assert!(!is_mix_pam_iq(&iq2));
}

#[test]
fn pam_roster_upsert_get_remove() {
    let mut roster = MixRoster::new();
    let user: jid::BareJid = "alice@example.com".parse().unwrap();
    let channel: jid::BareJid = "general@mix.example.com".parse().unwrap();
    roster.upsert(
        MixSubscription::new(user.clone(), channel.clone(), "pid-1")
            .with_nick("Alice")
            .with_nodes(["urn:xmpp:mix:nodes:messages".into()]),
    );
    assert!(roster.contains(&channel));
    assert_eq!(roster.get(&channel).unwrap().nick.as_deref(), Some("Alice"));
    roster.upsert(
        MixSubscription::new(user, channel.clone(), "pid-1")
            .with_nick("Ally")
            .with_nodes(["urn:xmpp:mix:nodes:messages".into()]),
    );
    assert_eq!(roster.get(&channel).unwrap().nick.as_deref(), Some("Ally"));
    assert!(roster.remove(&channel).is_some());
    assert!(!roster.contains(&channel));
}

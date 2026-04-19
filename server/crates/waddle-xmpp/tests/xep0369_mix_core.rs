//! XEP-0369: Mediated Information eXchange (MIX) — core conformance suite.
//!
//! Exercises the typed request/response model for the four MIX-core IQs:
//! `<join/>`, `<leave/>`, `<setnick/>`, `<update-subscription/>`, plus the
//! channel-side state machine.

use minidom::Element;
use waddle_xmpp::mix::{
    build_join_result, build_leave_result, build_setnick_result, build_update_subscription_result,
    channel::MixLeaf, parse_join, parse_leave, parse_setnick, parse_update_subscription,
    MixChannel, MixChannelConfig, MixChannelRegistry, MixError, Participant, NS_MIX_CORE,
};
use waddle_xmpp::xep::xep0369::is_mix_core_iq;
use xmpp_parsers::iq::{Iq, IqType};

fn iq_set(child: Element) -> Iq {
    Iq {
        from: Some("alice@example.com/r".parse().unwrap()),
        to: Some("general@mix.example.com".parse().unwrap()),
        id: "iq-mix-1".into(),
        payload: IqType::Set(child),
    }
}

#[test]
fn mix_join_round_trip_with_subscribes() {
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
    let iq = iq_set(elem);
    let parsed = parse_join(&iq).unwrap();
    assert_eq!(parsed.nick.as_deref(), Some("Alice"));
    assert_eq!(parsed.subscribe.len(), 2);

    let channel: jid::BareJid = "general@mix.example.com".parse().unwrap();
    let result = build_join_result(&iq, "participant-007", &channel, &parsed.subscribe);
    match result.payload {
        IqType::Result(Some(e)) => {
            assert_eq!(e.attr("id"), Some("participant-007"));
            assert_eq!(e.attr("jid"), Some("general@mix.example.com"));
            assert_eq!(e.children().count(), 2);
        }
        _ => panic!("expected result"),
    }
}

#[test]
fn mix_join_rejects_unknown_leaf() {
    let elem = Element::builder("join", NS_MIX_CORE)
        .append(
            Element::builder("subscribe", NS_MIX_CORE)
                .attr("node", "urn:xmpp:mix:nodes:bogus")
                .build(),
        )
        .build();
    assert!(matches!(
        parse_join(&iq_set(elem)),
        Err(MixError::InvalidLeaf(_))
    ));
}

#[test]
fn mix_leave_and_setnick_round_trip() {
    let leave = iq_set(Element::builder("leave", NS_MIX_CORE).build());
    assert!(parse_leave(&leave).is_ok());
    let channel: jid::BareJid = "general@mix.example.com".parse().unwrap();
    let leave_res = build_leave_result(&leave, &channel);
    matches!(leave_res.payload, IqType::Result(None));

    let setnick = iq_set(
        Element::builder("setnick", NS_MIX_CORE)
            .append(Element::builder("nick", NS_MIX_CORE).append("Ally").build())
            .build(),
    );
    let parsed = parse_setnick(&setnick).unwrap();
    assert_eq!(parsed.nick, "Ally");
    let setnick_res = build_setnick_result(&setnick, &channel, &parsed.nick);
    matches!(setnick_res.payload, IqType::Result(Some(_)));
}

#[test]
fn mix_update_subscription_round_trip() {
    let iq = iq_set(
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
    let channel: jid::BareJid = "general@mix.example.com".parse().unwrap();
    let res = build_update_subscription_result(&iq, &channel, &parsed.subscribe);
    matches!(res.payload, IqType::Result(Some(_)));
}

#[test]
fn mix_channel_lifecycle() {
    let channel_jid: jid::BareJid = "general@mix.example.com".parse().unwrap();
    let mut channel = MixChannel::new(
        channel_jid,
        "waddle-1".into(),
        "channel-1".into(),
        MixChannelConfig::default(),
    );
    let alice: jid::BareJid = "alice@example.com".parse().unwrap();
    channel.upsert_participant(Participant::new(alice.clone(), "Alice"));
    assert_eq!(channel.participant_count(), 1);

    channel.set_nick(&alice, "Ally".into());
    assert_eq!(channel.get_participant(&alice).unwrap().nick, "Ally");

    channel.update_subscription(&alice, &[MixLeaf::Config], &[MixLeaf::Info]);
    let sub = &channel.get_participant(&alice).unwrap().subscription;
    assert!(sub.contains(MixLeaf::Config));
    assert!(!sub.contains(MixLeaf::Info));

    channel.remove_participant(&alice);
    assert_eq!(channel.participant_count(), 0);
}

#[test]
fn mix_channel_registry_concurrency_shape() {
    let reg = MixChannelRegistry::new("mix.example.com".into());
    let jid: jid::BareJid = "general@mix.example.com".parse().unwrap();
    let handle = reg
        .get_or_create(
            jid.clone(),
            "w".into(),
            "c".into(),
            MixChannelConfig::default(),
        )
        .unwrap();
    assert_eq!(handle.channel_jid, jid);
    // Idempotent:
    let _ = reg
        .get_or_create(
            jid.clone(),
            "w".into(),
            "c".into(),
            MixChannelConfig::default(),
        )
        .unwrap();
    assert_eq!(reg.channel_count(), 1);
    assert!(reg.destroy(&jid));
    assert!(!reg.exists(&jid));
}

#[test]
fn mix_core_iq_recognition() {
    let iq = iq_set(Element::builder("join", NS_MIX_CORE).build());
    assert!(is_mix_core_iq(&iq));

    let foreign = iq_set(Element::builder("join", "other").build());
    assert!(!is_mix_core_iq(&foreign));
}

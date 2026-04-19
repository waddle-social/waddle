//! XEP-0407: MIX Miscellaneous Capabilities — dedicated suite.

use jid::BareJid;
use minidom::Element;
use waddle_xmpp::mix::{NS_MIX_CORE, NS_MIX_MISC, NS_MIX_PAM};
use waddle_xmpp::xep::xep0407::{
    build_invitation_element, build_invite_result, is_mix_misc_iq, mix_disco_features,
    parse_invitation, parse_invite_request, set_invitation_on_message, Invitation, MiscError,
};
use xmpp_parsers::iq::{Iq, IqType};

fn iq_set(child: Element) -> Iq {
    Iq {
        from: Some("alice@example.com/r".parse().unwrap()),
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
fn mix_misc_invite_flow() {
    let invite = Element::builder("invite", NS_MIX_MISC)
        .append(
            Element::builder("invitee", NS_MIX_MISC)
                .append("bob@example.com")
                .build(),
        )
        .build();
    let parsed = parse_invite_request(&iq_set(invite.clone())).unwrap();
    assert_eq!(parsed.invitee.to_string(), "bob@example.com");

    let result = build_invite_result(&iq_set(invite), &sample_invitation());
    match result.payload {
        IqType::Result(Some(ref e)) => assert!(e.is("invitation", NS_MIX_MISC)),
        _ => panic!("expected result"),
    }
}

#[test]
fn mix_misc_invitation_round_trip() {
    let inv = sample_invitation();
    let elem = build_invitation_element(&inv);
    let parsed = parse_invitation(&elem).unwrap();
    assert_eq!(parsed, inv);
}

#[test]
fn mix_misc_invitation_rejects_empty_token() {
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
fn mix_misc_set_invitation_on_message() {
    use xmpp_parsers::message::Message;
    let mut msg = Message::new(Some(jid::Jid::from(
        "bob@example.com".parse::<BareJid>().unwrap(),
    )));
    set_invitation_on_message(&mut msg, &sample_invitation());
    assert!(msg.payloads.iter().any(|p| p.is("invitation", NS_MIX_MISC)));
}

#[test]
fn mix_misc_is_iq_recognition() {
    assert!(is_mix_misc_iq(&iq_set(
        Element::builder("invite", NS_MIX_MISC)
            .append(
                Element::builder("invitee", NS_MIX_MISC)
                    .append("b@example.com")
                    .build()
            )
            .build()
    )));
    assert!(!is_mix_misc_iq(&iq_set(
        Element::builder("invite", "other").build()
    )));
}

#[test]
fn mix_disco_features_include_core_pam_misc() {
    let feats = mix_disco_features();
    assert!(feats.contains(&NS_MIX_CORE));
    assert!(feats.contains(&NS_MIX_PAM));
    assert!(feats.contains(&NS_MIX_MISC));
}

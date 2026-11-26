use jid::{DomainPart, FullJid, Jid};
use minidom::rxml::xml_ncname;
use minidom::Element;
use waddle_server::credential_iq::stanza_to_xml;
use waddle_server::credential_iq::{
    bounce_iq_error_xml_with_payload, bounce_outgoing_block_frames, bounce_undeliverable_iq_reply,
    bounce_unroutable_iq_reply,
};
use waddle_server::destination::{without_server_minted_credentials, UnroutableReason};
use waddle_xmpp::stream_management::StreamManagementState;
use waddle_xmpp::telemetry::attributes::SmEvictionPath;
use waddle_xmpp::waddle_credential::NS_WADDLE_CREDENTIAL;
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

const SENTINEL: &str = "sk-waddle-m3-bounce-sentinel-9f8e7d6c5b4a";

fn cred_iq(id: &str, payload: Element) -> Iq {
    Iq::Set {
        from: Some("alice@example.com/web".parse::<Jid>().expect("from")),
        to: Some("bob@example.com/phone".parse::<Jid>().expect("to")),
        id: id.to_owned(),
        payload,
    }
}

#[test]
fn m3_all_four_bounce_paths_strip_credentials_and_keep_sm_unacked_clean() {
    let top_cred = Element::builder("enroll-credential", NS_WADDLE_CREDENTIAL)
        .append(
            Element::builder("sealed-payload", NS_WADDLE_CREDENTIAL)
                .append(SENTINEL)
                .build(),
        )
        .build();
    let iq = cred_iq("bounce-m3-1", top_cred.clone());

    let mut sm = StreamManagementState::new();
    sm.enable("sm-m3-test".to_owned(), true, Some(300));

    // Path 1: route_to_connection::undeliverable_iq_reply
    let p1 = bounce_undeliverable_iq_reply(&Stanza::Iq(Box::new(iq.clone()))).expect("p1 reply");
    let p1_xml = stanza_to_xml(&p1);
    assert!(!p1_xml.contains(SENTINEL));
    assert!(!p1_xml.contains(NS_WADDLE_CREDENTIAL));
    let _ = sm.record_outbound(p1_xml, SmEvictionPath::DirectOutbound);

    // Path 2: destination::unroutable_iq_reply + without_server_minted_credentials
    let sender: Jid = "alice@example.com/web".parse().expect("sender");
    let home = DomainPart::new("example.com").expect("home");
    let p2 = bounce_unroutable_iq_reply(&iq, &sender, &home, UnroutableReason::FederationDisabled)
        .expect("p2 reply");
    let p2_xml = stanza_to_xml(&Stanza::Iq(Box::new(p2)));
    assert!(!p2_xml.contains(SENTINEL));
    assert!(!p2_xml.contains(NS_WADDLE_CREDENTIAL));
    let _ = sm.record_outbound(p2_xml, SmEvictionPath::DirectOutbound);

    let stripped_s2s = without_server_minted_credentials(iq.clone());
    let s2s_xml = stanza_to_xml(&Stanza::Iq(Box::new(stripped_s2s)));
    assert!(!s2s_xml.contains(SENTINEL));

    // Path 3: full_jid_forward::outgoing_block_frames
    let sender_full: FullJid = "alice@example.com/web".parse().expect("full");
    let target: Jid = "bob@example.com/phone".parse().expect("target");
    let p3_frames = bounce_outgoing_block_frames(&iq, &sender_full, &target);
    assert_eq!(p3_frames.len(), 1);
    assert!(!p3_frames[0].contains(SENTINEL));
    assert!(!p3_frames[0].contains(NS_WADDLE_CREDENTIAL));
    let _ = sm.record_outbound(p3_frames[0].clone(), SmEvictionPath::DirectOutbound);

    // Path 4: transport_xml::build_iq_error_xml_with_payload (muc_admin)
    let err = StanzaError::new(
        ErrorType::Auth,
        DefinedCondition::Forbidden,
        "en",
        "forbidden",
    );
    let p4_xml = bounce_iq_error_xml_with_payload(
        "bounce-m3-4",
        Some("room@muc.example.com"),
        Some("alice@example.com/web"),
        top_cred,
        err,
    );
    assert!(!p4_xml.contains(SENTINEL));
    assert!(!p4_xml.contains(NS_WADDLE_CREDENTIAL));
    let _ = sm.record_outbound(p4_xml, SmEvictionPath::DirectOutbound);

    // Non-regression: benign muc#admin payload is preserved.
    let benign = Element::builder("query", "http://jabber.org/protocol/muc#admin")
        .append(
            Element::builder("item", "http://jabber.org/protocol/muc#admin")
                .attr(xml_ncname!("affiliation").to_owned(), "member")
                .build(),
        )
        .build();
    let benign_xml = bounce_iq_error_xml_with_payload(
        "benign-1",
        Some("room@muc.example.com"),
        Some("alice@example.com/web"),
        benign,
        StanzaError::new(ErrorType::Auth, DefinedCondition::Forbidden, "en", "denied"),
    );
    assert!(benign_xml.contains("http://jabber.org/protocol/muc#admin"));
    assert!(benign_xml.contains("affiliation"));

    // Verify XEP-0198 sm_unacked contains zero occurrences of SENTINEL.
    assert!(sm.unacked_count() >= 4);
    for replay in sm.stanzas_to_resend(0) {
        assert!(!replay.stanza_xml.contains(SENTINEL));
        assert!(!replay.stanza_xml.contains(NS_WADDLE_CREDENTIAL));
    }
}

//! XEP-0490: Message Displayed Synchronization dedicated suite.
//!
//! Covers the helpers added in `waddle_xmpp::xep::xep0490` and the
//! well-known PEP node + access-model wiring in `PepHandler` /
//! `NodeConfig`.

use waddle_xmpp::pubsub::{AccessModel, NodeConfig, PepHandler, SendLastPublishedItem};
use waddle_xmpp::xep::xep0490::{
    build_displayed_element, is_displayed_element, parse_displayed_element, MdsDisplayed,
    MdsDisplayedError, StanzaId, NS_MDS_DISPLAYED, NS_MDS_DISPLAYED_NOTIFY, PEP_NODE_MDS_DISPLAYED,
};

fn jid(s: &str) -> jid::BareJid {
    s.parse().expect("valid bare jid")
}

#[test]
fn xep0490_well_known_node_is_supported() {
    assert!(PepHandler::is_well_known_node(PEP_NODE_MDS_DISPLAYED));
}

#[test]
fn xep0490_node_string_matches_namespace_string() {
    // XEP-0490 §3 uses the same URI for the PEP node and the
    // payload namespace; clients depend on this equality when they
    // disco-info+notify off the namespace.
    assert_eq!(PEP_NODE_MDS_DISPLAYED, NS_MDS_DISPLAYED);
}

#[test]
fn xep0490_notify_filter_is_namespace_plus_notify() {
    assert_eq!(
        NS_MDS_DISPLAYED_NOTIFY,
        format!("{NS_MDS_DISPLAYED}+notify")
    );
}

#[test]
fn xep0490_default_access_model_is_whitelist() {
    // XEP-0490 §3 mandates publish-options pin the node to whitelist.
    // Auto-create must apply that without needing the client form.
    assert_eq!(
        PepHandler::default_access_model_for_node(PEP_NODE_MDS_DISPLAYED),
        AccessModel::Whitelist
    );
}

#[test]
fn xep0490_node_config_defaults_match_spec_publish_options() {
    let config = NodeConfig::pep_for_node(PEP_NODE_MDS_DISPLAYED);
    // pubsub#access_model = whitelist
    assert_eq!(config.access_model, AccessModel::Whitelist);
    // pubsub#max_items = max (u32::MAX is the "no upper bound" form)
    assert_eq!(config.max_items, u32::MAX);
    // pubsub#persist_items = true
    assert!(config.persist_items);
    // pubsub#send_last_published_item = never
    assert_eq!(
        config.send_last_published_item,
        SendLastPublishedItem::Never
    );
}

#[test]
fn xep0490_displayed_element_round_trips_dm_shape() {
    let displayed = MdsDisplayed::new(
        StanzaId::new("0f710f2b-52ed-4d52-b928-784dad74a52b"),
        jid("juliet@capulet.lit"),
    );
    let elem = build_displayed_element(&displayed);
    assert!(is_displayed_element(&elem));
    let parsed = parse_displayed_element(&elem).expect("parse");
    assert_eq!(parsed, displayed);
}

#[test]
fn xep0490_displayed_element_round_trips_muc_shape() {
    let displayed = MdsDisplayed::new(
        StanzaId::new("ca21deaf-812c-48f1-8f16-339a674f2864"),
        jid("example@conference.shakespeare.lit"),
    );
    let elem = build_displayed_element(&displayed);
    let parsed = parse_displayed_element(&elem).expect("parse");
    assert_eq!(parsed, displayed);
    // by attribute MUST be the room bare JID — sanity-check the wire
    // shape so a future refactor that defaulted to "the user JID"
    // would trip this test instead of silently breaking MUC sync.
    assert_eq!(
        parsed.stanza_id_by.to_string(),
        "example@conference.shakespeare.lit"
    );
}

#[test]
fn xep0490_displayed_element_rejects_wrong_namespace() {
    let elem = minidom::Element::builder("displayed", "urn:xmpp:chat-markers:0").build();
    assert_eq!(
        parse_displayed_element(&elem).unwrap_err(),
        MdsDisplayedError::WrongElement
    );
}

#[test]
fn xep0490_displayed_element_rejects_missing_stanza_id() {
    let elem = minidom::Element::builder("displayed", NS_MDS_DISPLAYED).build();
    assert_eq!(
        parse_displayed_element(&elem).unwrap_err(),
        MdsDisplayedError::MissingStanzaId
    );
}

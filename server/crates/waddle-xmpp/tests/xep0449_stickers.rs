//! XEP-0449: Stickers — dedicated suite.
//!
//! Pins:
//! - the registrar namespace `urn:xmpp:stickers:0`,
//! - the `<sticker pack='...'/>` reference shape inside a message
//!   alongside a fallback `<body/>` and a XEP-0447 `<file-sharing/>`
//!   payload (xep-0449.xml "Sending a sticker"),
//! - build → serialize → reparse round-trips,
//! - extraction robustness (missing/empty `pack`, wrong namespace),
//! - `StickerPack` builder invariants.

use minidom::Element;
use waddle_xmpp::xep::xep0447::{extract_file_sharing_from_message, Disposition, NS_SFS};
use waddle_xmpp::xep::xep0449::{
    build_sticker_element, build_sticker_message, extract_sticker_ref, is_sticker_element,
    is_sticker_message, set_sticker_ref, strip_sticker_ref, Sticker, StickerCarrier, StickerPack,
    StickerRef, NS_STICKERS,
};
use xmpp_parsers::message::{Message, MessageType};

// ── Namespace exactness ──────────────────────────────────────────────

#[test]
fn xep0449_namespace_matches_spec() {
    assert_eq!(NS_STICKERS, "urn:xmpp:stickers:0");
}

// ── Wire-shape round-trips ───────────────────────────────────────────

#[test]
fn xep0449_sticker_element_survives_serialize_reparse_round_trip() {
    let elem = build_sticker_element(&StickerRef::new("EpRv28DHHzFrE4zd+xaNpVb4"));
    let xml = String::from(&elem);
    let reparsed: Element = xml.parse().expect("built element must reparse");

    assert!(is_sticker_element(&reparsed));
    assert_eq!(reparsed.attr("pack"), Some("EpRv28DHHzFrE4zd+xaNpVb4"));
}

#[test]
fn xep0449_full_sticker_message_carries_body_ref_and_file_sharing() {
    // xep-0449.xml "Sending a sticker": the message carries a fallback
    // body, the `<sticker/>` reference, and a XEP-0447 file-sharing
    // payload so non-supporting clients still render something.
    let to: jid::Jid = "room@muc.example.com".parse().expect("valid jid");
    let msg = build_sticker_message(
        to.clone(),
        &StickerRef::new("https://example.com/pack1"),
        ":thumbsup:",
        "https://example.com/thumbsup.webp",
        "image/webp",
    );

    assert_eq!(msg.to, Some(to));
    assert_eq!(msg.type_, MessageType::Groupchat);
    assert!(msg.id.is_some(), "sticker messages must carry an id");
    assert!(is_sticker_message(&msg));
    assert!(!msg.bodies.is_empty(), "fallback body is mandatory");

    let sharing = extract_file_sharing_from_message(&msg).expect("file-sharing payload present");
    assert_eq!(sharing.disposition, Some(Disposition::Inline));
    assert_eq!(sharing.metadata.media_type.as_deref(), Some("image/webp"));
    assert_eq!(
        sharing.first_url(),
        Some("https://example.com/thumbsup.webp")
    );
}

#[test]
fn xep0449_sticker_message_survives_serialize_reparse_round_trip() {
    let to: jid::Jid = "room@muc.example.com".parse().expect("valid jid");
    let msg = build_sticker_message(
        to,
        &StickerRef::new("pack-rt"),
        ":wave:",
        "https://example.com/wave.webp",
        "image/webp",
    );

    let elem = Element::from(msg);
    let xml = String::from(&elem);
    let reparsed =
        Message::try_from(xml.parse::<Element>().expect("reparses")).expect("valid message");

    assert!(reparsed.is_sticker());
    assert_eq!(
        reparsed.sticker_ref().map(|s| s.pack),
        Some("pack-rt".to_owned())
    );
    assert!(reparsed
        .payloads
        .iter()
        .any(|e| e.ns() == NS_SFS && e.name() == "file-sharing"));
}

// ── Extraction robustness ────────────────────────────────────────────

#[test]
fn xep0449_extract_rejects_missing_pack_attribute() {
    // The spec example `<sticker xmlns='urn:xmpp:stickers:0'/>` (no
    // pack attribute) still marks a sticker message; Waddle's typed
    // ref requires a pack id, so extraction yields None while the
    // element classifier still recognises the payload.
    let msg = Message::try_from(
        "<message xmlns='jabber:client' type='chat'>\
            <sticker xmlns='urn:xmpp:stickers:0'/>\
        </message>"
            .parse::<Element>()
            .expect("valid xml"),
    )
    .expect("valid message");

    assert!(is_sticker_message(&msg), "classifier keys on the element");
    assert!(
        extract_sticker_ref(&msg).is_none(),
        "typed ref requires a pack id"
    );
}

#[test]
fn xep0449_extract_rejects_empty_pack_attribute() {
    let msg = Message::try_from(
        "<message xmlns='jabber:client' type='chat'>\
            <sticker xmlns='urn:xmpp:stickers:0' pack=''/>\
        </message>"
            .parse::<Element>()
            .expect("valid xml"),
    )
    .expect("valid message");
    assert!(extract_sticker_ref(&msg).is_none());
}

#[test]
fn xep0449_wrong_namespace_sticker_is_not_recognized() {
    let msg = Message::try_from(
        "<message xmlns='jabber:client' type='chat'>\
            <sticker xmlns='urn:xmpp:stickers:1' pack='p'/>\
        </message>"
            .parse::<Element>()
            .expect("valid xml"),
    )
    .expect("valid message");

    assert!(!is_sticker_message(&msg));
    assert!(!msg.is_sticker());
    assert!(extract_sticker_ref(&msg).is_none());

    let wrong_name = Element::builder("stickers", NS_STICKERS).build();
    assert!(!is_sticker_element(&wrong_name));
}

// ── Mutation semantics ───────────────────────────────────────────────

#[test]
fn xep0449_set_replaces_prior_ref_keeping_exactly_one() {
    let mut msg = Message::new(None::<jid::Jid>);
    set_sticker_ref(&mut msg, &StickerRef::new("pack-1"));
    set_sticker_ref(&mut msg, &StickerRef::new("pack-2"));

    assert_eq!(
        msg.payloads
            .iter()
            .filter(|e| e.ns() == NS_STICKERS)
            .count(),
        1
    );
    assert_eq!(
        extract_sticker_ref(&msg).map(|s| s.pack),
        Some("pack-2".to_owned())
    );

    strip_sticker_ref(&mut msg);
    assert!(!is_sticker_message(&msg));
}

// ── Pack model invariants ────────────────────────────────────────────

#[test]
fn xep0449_pack_builder_preserves_sticker_order_and_fields() {
    let pack = StickerPack::new("pack-1", "Marsey the Cat")
        .with_sticker(
            Sticker::new(":thumbsup:")
                .with_url("https://example.com/thumbsup.webp")
                .with_media_type("image/webp")
                .with_hash("gw+6xdCg"),
        )
        .with_sticker(Sticker::new(":wave:"));

    assert_eq!(pack.id, "pack-1");
    assert_eq!(pack.name, "Marsey the Cat");
    assert_eq!(pack.len(), 2);
    assert!(!pack.is_empty());
    assert_eq!(pack.stickers[0].suggest, ":thumbsup:");
    assert_eq!(pack.stickers[0].hash.as_deref(), Some("gw+6xdCg"));
    assert_eq!(pack.stickers[1].suggest, ":wave:");
    assert_eq!(pack.stickers[1].url, None);
}

#[test]
fn xep0449_empty_pack_is_empty() {
    let pack = StickerPack::new("empty", "Empty");
    assert!(pack.is_empty());
    assert_eq!(pack.len(), 0);
}

// ── PEP sticker-pack node configuration ──────────────────────────────

mod pep_node {
    use waddle_xmpp_core::pubsub::node::{
        AccessModel, NodeConfig, PublishModel, SendLastPublishedItem, PEP_BOOKMARK_MAX_ITEMS,
    };
    use waddle_xmpp_core::pubsub::pep::{PepHandler, PEP_NODE_STICKERS};

    #[test]
    fn xep0449_pep_node_constant_matches_registrar_namespace() {
        // XEP-0449: the PEP node id IS the payload namespace. The core
        // constant and the wire-layer constant must never drift.
        assert_eq!(PEP_NODE_STICKERS, super::NS_STICKERS);
    }

    #[test]
    fn xep0449_sticker_node_is_well_known_with_open_access() {
        assert!(PepHandler::is_well_known_node(PEP_NODE_STICKERS));
        assert_eq!(
            PepHandler::default_access_model_for_node(PEP_NODE_STICKERS),
            AccessModel::Open
        );
    }

    #[test]
    fn xep0449_pep_for_node_yields_sticker_defaults() {
        // The auto-create path on a publish to `urn:xmpp:stickers:0`
        // must land the XEP-0449 shape: open reads (other users fetch
        // referenced packs), owner-only writes, many persistent items
        // under the finite bookmarks anti-DoS cap, and no presence
        // fan-out of multi-kilobyte pack payloads.
        let config = NodeConfig::pep_for_node(PEP_NODE_STICKERS);
        assert_eq!(config, NodeConfig::stickers_defaults());
        assert_eq!(config.access_model, AccessModel::Open);
        assert_eq!(config.publish_model, PublishModel::Publishers);
        assert_eq!(config.max_items, PEP_BOOKMARK_MAX_ITEMS);
        assert!(config.persist_items);
        assert_eq!(
            config.send_last_published_item,
            SendLastPublishedItem::Never
        );
    }
}

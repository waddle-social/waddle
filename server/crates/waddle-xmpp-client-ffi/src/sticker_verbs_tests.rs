//! XEP-0449 FFI test suite: sticker verb wire shapes as produced by
//! the exact FFI construction path, reply → typed-result mapping
//! (including the `item-not-found` ⇒ empty first-run state), pack-id
//! recomputation on publish, and the exported methods' typed failure
//! behavior (not-connected, invalid input) through a recording
//! listener.
//!
//! NOTE: the xep-0449.xml publish example elides pack items behind
//! `<!-- ... -->`, so the literal id it prints
//! (`EpRv28DHHzFrE4zd+xaNpVb4`) is not derivable from the visible
//! content. The recomputation tests pin the id computed from the
//! example's *visible* content (`MARSEY_VISIBLE_ID`), cross-checked
//! against an independent reference implementation, and use the XEP's
//! literal id only as an opaque wire token.

use std::sync::{Arc, Mutex as StdMutex};

use jid::BareJid;
use minidom::Element;

use waddle_xmpp_client::error::{StanzaError, StanzaErrorType};
use waddle_xmpp_client::stickers::{PackId, STICKERS_NODE};
use waddle_xmpp_client::ClientError;

use crate::convert::sticker_pack_from_ffi;
use crate::sticker_verbs::{
    map_fetch_pack_reply, map_fetch_packs_reply, map_retract_reply, sticker_pack_fetch_stanza,
    sticker_pack_publish_stanza, sticker_pack_retract_stanza, sticker_packs_fetch_stanza,
};
use crate::{
    WaddleClient, WaddleClientEvent, WaddleConfig, WaddleError, WaddleEventListener,
    WaddlePackSticker, WaddleStickerHash, WaddleStickerPack,
};

const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";
const NS_STICKERS: &str = "urn:xmpp:stickers:0";
const NS_HASHES: &str = "urn:xmpp:hashes:2";

/// Pack id of the XEP example's visible content, computed with the
/// spec-faithful §pack-hash algorithm (see module docs).
const MARSEY_VISIBLE_ID: &str = "N6zJI4PTGfFfSJbwakmBihwB";
/// The XEP's literal example id, used as an opaque wire token.
const XEP_EXAMPLE_ID: &str = "EpRv28DHHzFrE4zd+xaNpVb4";

#[derive(Clone, Default)]
struct RecordingListener {
    errors: Arc<StdMutex<Vec<String>>>,
}

impl RecordingListener {
    fn errors(&self) -> Vec<String> {
        self.errors.lock().expect("test mutex poisoned").clone()
    }
}

impl WaddleEventListener for RecordingListener {
    fn on_event(&self, event: WaddleClientEvent) {
        if let WaddleClientEvent::Error { description } = event {
            self.errors
                .lock()
                .expect("test mutex poisoned")
                .push(description);
        }
    }
}

fn test_client(listener: RecordingListener) -> Arc<WaddleClient> {
    Arc::new(WaddleClient {
        config: WaddleConfig {
            server_url: "wss://xmpp.waddle.test".to_string(),
            jid: "alice@waddle.test".to_string(),
            access_token: "token".to_string(),
            resource: "test".to_string(),
            resume_state: None,
        },
        listener: Arc::new(Box::new(listener) as Box<dyn WaddleEventListener>),
        handle: tokio::sync::Mutex::new(None),
        inbox_query_gate: tokio::sync::Mutex::new(()),
    })
}

fn bare(value: &str) -> BareJid {
    value.parse().expect("test bare JID parses")
}

fn pack_id(value: &str) -> PackId {
    PackId::new(value).expect("test pack id")
}

fn marsey_ffi_pack(id: &str) -> WaddleStickerPack {
    WaddleStickerPack {
        id: id.to_string(),
        name: Some("Marsey the Cat".to_string()),
        summary: Some("Be cute or be cynical, this little kitten works both ways.".to_string()),
        restricted: false,
        stickers: vec![
            WaddlePackSticker {
                desc: "👍".to_string(),
                media_type: Some("image/png".to_string()),
                size: Some(71045),
                width: Some(512),
                height: Some(512),
                hashes: vec![WaddleStickerHash {
                    algo: "sha-256".to_string(),
                    value_b64: "0AdP8lJOWJrugSKOIAqfEKqFatIpG5JBCjjxY253ojQ=".to_string(),
                }],
                sources: vec![
                    "https://download.montague.lit/51078299-d071-46e1-b6d3-3de4a8ab67d6/sticker_marsey_thumbs_up.png"
                        .to_string(),
                ],
                suggests: vec!["+1".to_string()],
            },
            WaddlePackSticker {
                desc: "😘".to_string(),
                media_type: Some("image/png".to_string()),
                size: Some(67016),
                width: Some(512),
                height: Some(512),
                hashes: vec![WaddleStickerHash {
                    algo: "sha-256".to_string(),
                    value_b64: "gw+6xdCgOcvCYSKuQNrXH33lV9NMzuDf/s0huByCDsY=".to_string(),
                }],
                sources: vec![
                    "https://download.montague.lit/51078299-d071-46e1-b6d3-3de4a8ab67d6/sticker_marsey_kiss.png"
                        .to_string(),
                ],
                suggests: vec![],
            },
        ],
    }
}

fn items_result_with_pack(node: &str, item_id: &str) -> Element {
    let doc = sticker_pack_from_ffi(marsey_ffi_pack(item_id)).expect("fixture converts");
    let pack = waddle_xmpp_client::stickers::build_pack_element(&doc);
    Element::builder("iq", "jabber:client")
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "fetch-1")
        .append(
            Element::builder("pubsub", NS_PUBSUB)
                .append(
                    Element::builder("items", NS_PUBSUB)
                        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
                        .append(
                            Element::builder("item", NS_PUBSUB)
                                .attr(minidom::rxml::xml_ncname!("id").to_owned(), item_id)
                                .append(pack)
                                .build(),
                        )
                        .build(),
                )
                .build(),
        )
        .build()
}

fn item_not_found() -> ClientError {
    ClientError::StanzaError(StanzaError {
        error_type: StanzaErrorType::Cancel,
        condition: "item-not-found".to_string(),
        text: None,
        application_condition: None,
    })
}

fn forbidden() -> ClientError {
    ClientError::StanzaError(StanzaError {
        error_type: StanzaErrorType::Auth,
        condition: "forbidden".to_string(),
        text: Some("nope".to_string()),
        application_condition: None,
    })
}

// ── Wire shapes ──────────────────────────────────────────────────────

#[test]
fn fetch_all_stanza_routes_to_own_pep_without_to() {
    let iq = sticker_packs_fetch_stanza(None);
    assert_eq!(iq.attr("type"), Some("get"));
    assert_eq!(iq.attr("to"), None, "own PEP: no to attribute");
    let items = iq
        .get_child("pubsub", NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("items", NS_PUBSUB))
        .expect("items child");
    assert_eq!(items.attr("node"), Some(STICKERS_NODE));
    assert_eq!(
        items.children().count(),
        0,
        "whole-node fetch has no item ids"
    );
}

#[test]
fn fetch_all_stanza_targets_another_owner_when_given() {
    let iq = sticker_packs_fetch_stanza(Some(&bare("romeo@montague.lit")));
    assert_eq!(iq.attr("to"), Some("romeo@montague.lit"));
}

#[test]
fn verb_stanzas_mint_fresh_uuid_iq_ids() {
    let first = sticker_packs_fetch_stanza(None);
    let second = sticker_packs_fetch_stanza(None);
    let first_id = first.attr("id").expect("iq id");
    let second_id = second.attr("id").expect("iq id");
    assert_ne!(first_id, second_id, "iq ids must never be fixed strings");
    assert!(uuid::Uuid::parse_str(first_id).is_ok(), "iq id is a UUID");
}

#[test]
fn fetch_one_stanza_carries_node_and_item_id() {
    let iq = sticker_pack_fetch_stanza(
        &bare("romeo@montague.lit"),
        "generic/sticker/node",
        &pack_id(XEP_EXAMPLE_ID),
    );
    assert_eq!(iq.attr("type"), Some("get"));
    assert_eq!(iq.attr("to"), Some("romeo@montague.lit"));
    let items = iq
        .get_child("pubsub", NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("items", NS_PUBSUB))
        .expect("items child");
    assert_eq!(items.attr("node"), Some("generic/sticker/node"));
    assert_eq!(
        items
            .get_child("item", NS_PUBSUB)
            .and_then(|item| item.attr("id")),
        Some(XEP_EXAMPLE_ID)
    );
}

#[test]
fn publish_stanza_recomputes_the_pack_id_from_content() {
    // The caller's id is display state; the pubsub item id MUST be the
    // §pack-hash derivation over the pack content — pinned against the
    // reference vector for the XEP example's visible items.
    let doc = sticker_pack_from_ffi(marsey_ffi_pack("caller-supplied-wrong-id")).expect("converts");
    let (id, iq) = sticker_pack_publish_stanza(&doc);
    assert_eq!(id.as_str(), MARSEY_VISIBLE_ID);

    assert_eq!(iq.attr("type"), Some("set"));
    assert_eq!(iq.attr("to"), None, "publish routes to own PEP");
    let publish = iq
        .get_child("pubsub", NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("publish", NS_PUBSUB))
        .expect("publish child");
    assert_eq!(publish.attr("node"), Some(STICKERS_NODE));
    let item = publish.get_child("item", NS_PUBSUB).expect("item child");
    assert_eq!(item.attr("id"), Some(MARSEY_VISIBLE_ID));
    assert_ne!(
        item.attr("id"),
        Some("current"),
        "packs are multi-item, never single-slot"
    );

    let pack = item.get_child("pack", NS_STICKERS).expect("pack payload");
    assert_eq!(
        pack.get_child("name", NS_STICKERS).map(|e| e.text()),
        Some("Marsey the Cat".to_string())
    );
    assert_eq!(
        pack.children()
            .filter(|child| child.is("item", NS_STICKERS))
            .count(),
        2
    );
    // Trailing pack-level hash agrees with the recomputed item id.
    let hash = pack
        .children()
        .filter(|child| child.is("hash", NS_HASHES))
        .last()
        .expect("pack-level hash");
    assert_eq!(hash.attr("algo"), Some("sha-256"));
    assert!(hash.text().starts_with(MARSEY_VISIBLE_ID));
}

#[test]
fn retract_stanza_targets_the_pack_item_on_the_sticker_node() {
    let iq = sticker_pack_retract_stanza(&pack_id(XEP_EXAMPLE_ID));
    assert_eq!(iq.attr("type"), Some("set"));
    assert_eq!(iq.attr("to"), None, "retract routes to own PEP");
    let retract = iq
        .get_child("pubsub", NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("retract", NS_PUBSUB))
        .expect("retract child");
    assert_eq!(retract.attr("node"), Some(STICKERS_NODE));
    assert_eq!(
        retract
            .get_child("item", NS_PUBSUB)
            .and_then(|item| item.attr("id")),
        Some(XEP_EXAMPLE_ID)
    );
}

// ── Reply mapping ────────────────────────────────────────────────────

#[test]
fn fetch_reply_maps_packs_into_ffi_records() {
    let reply = Ok(items_result_with_pack(STICKERS_NODE, MARSEY_VISIBLE_ID));
    let packs = map_fetch_packs_reply(reply, STICKERS_NODE).expect("maps");
    assert_eq!(packs.len(), 1);
    let pack = &packs[0];
    assert_eq!(pack.id, MARSEY_VISIBLE_ID);
    assert_eq!(pack.name.as_deref(), Some("Marsey the Cat"));
    assert!(!pack.restricted);
    assert_eq!(pack.stickers.len(), 2);
    assert_eq!(pack.stickers[0].desc, "👍");
    assert_eq!(pack.stickers[0].suggests, vec!["+1".to_string()]);
    assert_eq!(pack.stickers[0].hashes[0].algo, "sha-256");
    assert_eq!(pack.stickers[1].width, Some(512));
}

#[test]
fn item_not_found_is_the_empty_first_run_state() {
    let packs = map_fetch_packs_reply(Err(item_not_found()), STICKERS_NODE).expect("maps");
    assert!(packs.is_empty());
}

#[test]
fn other_stanza_errors_surface_typed() {
    let error =
        map_fetch_packs_reply(Err(forbidden()), STICKERS_NODE).expect_err("forbidden is an error");
    assert_eq!(
        error,
        WaddleError::Stanza {
            condition: "forbidden".to_string(),
            text: Some("nope".to_string()),
        }
    );

    let timeout = map_fetch_packs_reply(
        Err(ClientError::IqTimeout {
            timeout: std::time::Duration::from_secs(30),
        }),
        STICKERS_NODE,
    )
    .expect_err("timeout is an error");
    assert_eq!(timeout, WaddleError::Timeout);
}

#[test]
fn single_pack_reply_maps_found_and_missing() {
    let found = map_fetch_pack_reply(
        Ok(items_result_with_pack(STICKERS_NODE, MARSEY_VISIBLE_ID)),
        STICKERS_NODE,
        &pack_id(MARSEY_VISIBLE_ID),
    )
    .expect("maps");
    assert_eq!(
        found.map(|pack| pack.id),
        Some(MARSEY_VISIBLE_ID.to_string())
    );

    let missing = map_fetch_pack_reply(
        Ok(items_result_with_pack(STICKERS_NODE, MARSEY_VISIBLE_ID)),
        STICKERS_NODE,
        &pack_id("some-other-pack-id"),
    )
    .expect("maps");
    assert!(missing.is_none());

    let not_found = map_fetch_pack_reply(
        Err(item_not_found()),
        STICKERS_NODE,
        &pack_id(MARSEY_VISIBLE_ID),
    )
    .expect("item-not-found is Ok(None)");
    assert!(not_found.is_none());
}

#[test]
fn retract_item_not_found_is_idempotent_success() {
    // The pack is already gone (a cancelled-after-retract retry): the
    // delete self-heals — the caller reconciles its cache exactly as
    // on a fresh success.
    assert!(map_retract_reply(Err(item_not_found())).is_ok());

    // A plain result stays success; every other failure stays typed.
    let ok = Element::builder("iq", "jabber:client")
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "retract-1")
        .build();
    assert!(map_retract_reply(Ok(ok)).is_ok());
    assert!(map_retract_reply(Err(forbidden())).is_err());
    assert!(map_retract_reply(Err(ClientError::IqTimeout {
        timeout: std::time::Duration::from_secs(30),
    }))
    .is_err());
}

// ── Typed failures through the exported methods ──────────────────────

#[tokio::test]
async fn fetch_sticker_packs_reports_not_connected() {
    let listener = RecordingListener::default();
    let client = test_client(listener.clone());
    let result = client.fetch_sticker_packs(None).await;
    assert_eq!(result, Err(WaddleError::NotConnected));
    assert_eq!(listener.errors(), vec!["Not connected"]);
}

#[tokio::test]
async fn fetch_sticker_packs_rejects_invalid_owner_jid() {
    let listener = RecordingListener::default();
    let client = test_client(listener.clone());
    let result = client
        .fetch_sticker_packs(Some("not a jid".to_string()))
        .await;
    assert_eq!(result, Err(WaddleError::InvalidJid));
}

#[tokio::test]
async fn fetch_sticker_pack_reports_not_connected_and_invalid_input() {
    let listener = RecordingListener::default();
    let client = test_client(listener.clone());
    let result = client
        .fetch_sticker_pack(
            "romeo@montague.lit".to_string(),
            None,
            XEP_EXAMPLE_ID.to_string(),
        )
        .await;
    assert_eq!(result, Err(WaddleError::NotConnected));

    let invalid_jid = client
        .fetch_sticker_pack("not a jid".to_string(), None, XEP_EXAMPLE_ID.to_string())
        .await;
    assert_eq!(invalid_jid, Err(WaddleError::InvalidJid));

    let empty_id = client
        .fetch_sticker_pack("romeo@montague.lit".to_string(), None, "  ".to_string())
        .await;
    assert_eq!(empty_id, Err(WaddleError::InvalidArgument));
}

#[tokio::test]
async fn publish_sticker_pack_reports_not_connected() {
    let listener = RecordingListener::default();
    let client = test_client(listener.clone());
    let result = client.publish_sticker_pack(marsey_ffi_pack("any")).await;
    assert_eq!(result, Err(WaddleError::NotConnected));
    assert_eq!(listener.errors(), vec!["Not connected"]);
}

#[tokio::test]
async fn publish_sticker_pack_rejects_malformed_packs() {
    let listener = RecordingListener::default();
    let client = test_client(listener.clone());

    // Zero stickers: XEP-0449 packs contain "one or more" items.
    let mut empty = marsey_ffi_pack("any");
    empty.stickers.clear();
    assert_eq!(
        client.publish_sticker_pack(empty).await,
        Err(WaddleError::InvalidArgument)
    );

    // Unsupported hash algorithm never reaches the wire.
    let mut bad_algo = marsey_ffi_pack("any");
    bad_algo.stickers[0].hashes[0].algo = "md5".to_string();
    assert_eq!(
        client.publish_sticker_pack(bad_algo).await,
        Err(WaddleError::InvalidArgument)
    );

    let errors = listener.errors();
    assert_eq!(errors.len(), 2);
    assert!(errors[0].contains("at least one sticker"), "{errors:?}");
    assert!(
        errors[1].contains("unsupported hash algorithm"),
        "{errors:?}"
    );
}

#[tokio::test]
async fn retract_sticker_pack_reports_failures_as_false() {
    let listener = RecordingListener::default();
    let client = test_client(listener.clone());

    assert!(!client.retract_sticker_pack("   ".to_string()).await);
    assert!(
        !client
            .retract_sticker_pack(XEP_EXAMPLE_ID.to_string())
            .await
    );

    let errors = listener.errors();
    assert_eq!(errors.len(), 2);
    assert!(
        errors[0].contains("pack id must not be empty"),
        "{errors:?}"
    );
    assert_eq!(errors[1], "Not connected");
}

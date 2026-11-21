//! ADR-010 status-preference receive side over the FFI: the typed core
//! values map onto the FFI records, the live PEP event reaches the
//! listener, and a fetch result is trusted only from the account's own
//! PEP service.

use std::sync::{Arc, Mutex as StdMutex};

use jid::BareJid;
use minidom::Element;
use waddle_xmpp_client::status_preference::build_status_preference_publish_iq;
use waddle_xmpp_client::ClientEvent;
use waddle_xmpp_core::waddle_status_preference::{
    ManualStatus, StatusPreference, NS_WADDLE_STATUS_PREFERENCE, PEP_ITEM_WADDLE_STATUS_PREFERENCE,
    PEP_NODE_WADDLE_STATUS_PREFERENCE,
};

use crate::convert::dispatch_event;
use crate::status_preference::{
    status_preference_from_fetch_result, status_preference_from_ffi, status_preference_to_ffi,
};
use crate::{
    WaddleClient, WaddleClientEvent, WaddleConfig, WaddleError, WaddleEventListener,
    WaddleManualStatus, WaddleStatusPreference,
};

const NS_CLIENT: &str = "jabber:client";
const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";

#[derive(Clone, Default)]
struct CapturingListener {
    events: Arc<StdMutex<Vec<WaddleClientEvent>>>,
}

impl CapturingListener {
    fn events(&self) -> Vec<WaddleClientEvent> {
        self.events.lock().expect("test mutex poisoned").clone()
    }
}

impl WaddleEventListener for CapturingListener {
    fn on_event(&self, event: WaddleClientEvent) {
        self.events.lock().expect("test mutex poisoned").push(event);
    }
}

fn account() -> BareJid {
    "alice@waddle.test".parse().expect("bare JID")
}

fn fetch_result(from: Option<&str>, preference: Option<StatusPreference>) -> Element {
    let mut items = Element::builder("items", NS_PUBSUB).attr(
        minidom::rxml::xml_ncname!("node").to_owned(),
        PEP_NODE_WADDLE_STATUS_PREFERENCE,
    );
    if let Some(preference) = preference {
        items = items.append(
            Element::builder("item", NS_PUBSUB)
                .attr(
                    minidom::rxml::xml_ncname!("id").to_owned(),
                    PEP_ITEM_WADDLE_STATUS_PREFERENCE,
                )
                .append(preference.build_element())
                .build(),
        );
    }
    let mut iq = Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "sp-1");
    if let Some(from) = from {
        iq = iq.attr(minidom::rxml::xml_ncname!("from").to_owned(), from);
    }
    iq.append(
        Element::builder("pubsub", NS_PUBSUB)
            .append(items.build())
            .build(),
    )
    .build()
}

#[test]
fn every_core_preference_maps_onto_the_ffi_record() {
    assert_eq!(
        status_preference_to_ffi(StatusPreference::Automatic),
        WaddleStatusPreference::Automatic
    );
    for (core, ffi) in [
        (ManualStatus::Available, WaddleManualStatus::Available),
        (ManualStatus::Chat, WaddleManualStatus::Chat),
        (ManualStatus::Away, WaddleManualStatus::Away),
        (ManualStatus::Dnd, WaddleManualStatus::Dnd),
    ] {
        assert_eq!(
            status_preference_to_ffi(StatusPreference::Manual(core)),
            WaddleStatusPreference::Manual { status: ffi }
        );
    }
}

#[test]
fn dispatch_event_maps_the_live_pick_to_ffi() {
    let listener = CapturingListener::default();
    dispatch_event(
        ClientEvent::StatusPreferenceChanged(StatusPreference::Manual(ManualStatus::Away)),
        "alice@waddle.test",
        &listener,
    );

    match listener.events().as_slice() {
        [WaddleClientEvent::StatusPreferenceChanged { preference }] => assert_eq!(
            *preference,
            WaddleStatusPreference::Manual {
                status: WaddleManualStatus::Away
            }
        ),
        _ => panic!("expected one StatusPreferenceChanged event"),
    }
}

#[test]
fn a_fetch_from_the_own_pep_service_yields_the_stored_pick() {
    let manual_dnd = WaddleStatusPreference::Manual {
        status: WaddleManualStatus::Dnd,
    };
    for from in [None, Some("alice@waddle.test")] {
        let result = fetch_result(from, Some(StatusPreference::Manual(ManualStatus::Dnd)));
        assert_eq!(
            status_preference_from_fetch_result(&result, &account()),
            manual_dnd,
            "from {from:?}"
        );
    }
}

#[test]
fn an_empty_node_or_a_spoofed_result_is_automatic() {
    let empty = fetch_result(None, None);
    assert_eq!(
        status_preference_from_fetch_result(&empty, &account()),
        WaddleStatusPreference::Automatic
    );
    for from in [
        "mallory@waddle.test",
        "alice@waddle.test/phone",
        "not a jid@@",
    ] {
        let spoofed = fetch_result(
            Some(from),
            Some(StatusPreference::Manual(ManualStatus::Away)),
        );
        assert_eq!(
            status_preference_from_fetch_result(&spoofed, &account()),
            WaddleStatusPreference::Automatic,
            "from {from}"
        );
    }
}

#[test]
fn the_payload_namespace_is_the_waddle_node() {
    // Pinned so a rename cannot silently drop every stored pick.
    assert_eq!(
        NS_WADDLE_STATUS_PREFERENCE,
        "urn:waddle:status-preference:0"
    );
}

fn offline_client() -> Arc<WaddleClient> {
    Arc::new(WaddleClient {
        config: WaddleConfig {
            server_url: "wss://xmpp.waddle.test".to_string(),
            jid: "alice@waddle.test".to_string(),
            access_token: "token".to_string(),
            resource: "test".to_string(),
            resumable: false,
        },
        listener: Arc::new(Box::new(CapturingListener::default()) as Box<dyn WaddleEventListener>),
        handle: tokio::sync::Mutex::new(None),
        inbox_query_gate: tokio::sync::Mutex::new(()),
    })
}

#[tokio::test]
async fn fetch_without_a_session_is_not_connected() {
    assert_eq!(
        offline_client().fetch_status_preference().await,
        Err(WaddleError::NotConnected)
    );
}

#[test]
fn every_ffi_preference_maps_onto_the_core_value() {
    assert_eq!(
        status_preference_from_ffi(WaddleStatusPreference::Automatic),
        StatusPreference::Automatic
    );
    for (ffi, core) in [
        (WaddleManualStatus::Available, ManualStatus::Available),
        (WaddleManualStatus::Chat, ManualStatus::Chat),
        (WaddleManualStatus::Away, ManualStatus::Away),
        (WaddleManualStatus::Dnd, ManualStatus::Dnd),
    ] {
        assert_eq!(
            status_preference_from_ffi(WaddleStatusPreference::Manual { status: ffi }),
            StatusPreference::Manual(core)
        );
    }
}

/// ADR-010 compliance invariant 5: one item, id `current`, on the
/// account's own node, with no `<publish-options/>` (the server pins the
/// node owner-only as a well-known node).
#[test]
fn a_pick_is_published_as_the_current_item_of_the_own_node() {
    let iq =
        build_status_preference_publish_iq("sp-2", &StatusPreference::Manual(ManualStatus::Dnd));

    assert_eq!(iq.attr("type"), Some("set"));
    assert_eq!(iq.attr("id"), Some("sp-2"));
    assert_eq!(iq.attr("to"), None, "the account's own PEP service");
    let pubsub = iq.get_child("pubsub", NS_PUBSUB).expect("pubsub");
    assert!(pubsub.get_child("publish-options", NS_PUBSUB).is_none());
    let publish = pubsub.get_child("publish", NS_PUBSUB).expect("publish");
    assert_eq!(publish.attr("node"), Some("urn:waddle:status-preference:0"));
    let items: Vec<_> = publish.children().collect();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].attr("id"), Some("current"));
    let payload = items[0]
        .get_child("status-preference", NS_WADDLE_STATUS_PREFERENCE)
        .expect("payload");
    assert_eq!(payload.attr("mode"), Some("manual"));
    assert_eq!(payload.attr("status"), Some("dnd"));
}

/// A reset is published, not retracted, so it fans out to the account's
/// other devices as a value.
#[test]
fn a_reset_is_published_as_automatic_with_no_status() {
    let iq = build_status_preference_publish_iq("sp-3", &StatusPreference::Automatic);
    let payload = iq
        .get_child("pubsub", NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("publish", NS_PUBSUB))
        .and_then(|publish| publish.get_child("item", NS_PUBSUB))
        .and_then(|item| item.get_child("status-preference", NS_WADDLE_STATUS_PREFERENCE))
        .expect("payload");
    assert_eq!(payload.attr("mode"), Some("automatic"));
    assert_eq!(payload.attr("status"), None);
}

#[tokio::test]
async fn publish_without_a_session_is_not_connected() {
    let client = offline_client();
    assert_eq!(
        client
            .publish_status_preference(WaddleStatusPreference::Manual {
                status: WaddleManualStatus::Away
            })
            .await
            .unwrap_err(),
        WaddleError::NotConnected
    );
}

//! ADR-010 status-preference receive side over the FFI: the typed core
//! values map onto the FFI records, the live PEP event reaches the
//! listener, and a fetch result is trusted only from the account's own
//! PEP service.

use std::sync::{Arc, Mutex as StdMutex};

use jid::BareJid;
use minidom::Element;
use waddle_xmpp_client::ClientEvent;
use waddle_xmpp_core::waddle_status_preference::{
    ManualStatus, StatusPreference, NS_WADDLE_STATUS_PREFERENCE, PEP_ITEM_WADDLE_STATUS_PREFERENCE,
    PEP_NODE_WADDLE_STATUS_PREFERENCE,
};

use crate::convert::dispatch_event;
use crate::status_preference::{status_preference_from_fetch_result, status_preference_to_ffi};
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

#[tokio::test]
async fn fetch_without_a_session_is_not_connected() {
    let client = Arc::new(WaddleClient {
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
    });

    assert_eq!(
        client.fetch_status_preference().await,
        Err(WaddleError::NotConnected)
    );
}

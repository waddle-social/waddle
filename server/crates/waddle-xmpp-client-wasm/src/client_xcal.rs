//! xCal calendar events — wasm bridge surface.
//!
//! Wraps the typed `waddle_xmpp_core::xcal` model behind JS-friendly
//! shapes. Items are full `<vcalendar><vevent/></vcalendar>` payloads
//! per the XSF ProtoXEP "Calendaring Extensions to Publish-Subscribe";
//! recurrence flows through an `<rrule>` element with FREQ/INTERVAL/
//! BYDAY/BYMONTHDAY/COUNT|UNTIL.

use uuid::Uuid;
use waddle_xmpp_client::pubsub::build_pubsub_retract_iq;
use waddle_xmpp_client_community::calendar::{
    build_event_publish_iq, build_events_items_iq, build_item_publish_iq, build_rsvp_publish_iq,
    item_from_input, parse_events_items_result, vevent_from_input, WaddleAttendee,
    WaddleCalendarItemInput, WaddleVEvent, WaddleVEventInput,
};
use waddle_xmpp_core::xcal::{PartStat, PUBSUB_NODE_EVENTS};
use wasm_bindgen::prelude::{wasm_bindgen, JsValue};

use super::{js_error, send_iq_command, service_bare_jid, to_js_value, WaddleClient};

#[wasm_bindgen]
impl WaddleClient {
    /// Fetch the latest calendar events from the community events
    /// node. Returns ALL items including past events; chat-side
    /// composables filter by DTSTART for upcoming-only views.
    pub fn xcal_items(&self, community_jid: String, max_items: Option<u32>) -> js_sys::Promise {
        let inner = self.inner.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let service = service_bare_jid(&community_jid)?;
            let iq = build_events_items_iq(&service, max_items);
            let result = send_iq_command(inner, iq).await?;
            let events = parse_events_items_result(&result);
            to_js_value(&events)
        })
    }

    /// Publish a new calendar event, optionally with an RRULE for
    /// recurrence. SUMMARY is required (per RFC 5545); DTSTART is
    /// required for the event to be useful on a timeline.
    pub fn xcal_publish(&self, community_jid: String, input: JsValue) -> js_sys::Promise {
        let inner = self.inner.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let service = service_bare_jid(&community_jid)?;
            let input: WaddleVEventInput = serde_wasm_bindgen::from_value(input)
                .map_err(|err| js_error(format!("invalid event input: {err}")))?;
            let item_id = format!("evt-{}", Uuid::new_v4());
            let event = vevent_from_input(&item_id, input).map_err(|e| js_error(e.to_string()))?;
            let iq = build_event_publish_iq(&service, &item_id, &event);
            send_iq_command(inner, iq).await?;
            let js_event = WaddleVEvent::from_vevent(&item_id, event);
            to_js_value(&js_event)
        })
    }

    /// Publish (or replace) a full CalendarItem at the given item
    /// id — master event plus optional per-instance overrides and
    /// EXDATE cancellations. Use this for the read-modify-write
    /// flows ("edit this occurrence", "edit all occurrences",
    /// "cancel this occurrence") after fetching current state via
    /// `xcal_items`. Passing an existing item id overwrites that
    /// item atomically; passing a new id creates a new item.
    pub fn xcal_publish_item(
        &self,
        community_jid: String,
        item_id: String,
        input: JsValue,
    ) -> js_sys::Promise {
        let inner = self.inner.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let service = service_bare_jid(&community_jid)?;
            let input: WaddleCalendarItemInput = serde_wasm_bindgen::from_value(input)
                .map_err(|err| js_error(format!("invalid calendar item input: {err}")))?;
            let item = item_from_input(&item_id, input).map_err(|e| js_error(e.to_string()))?;
            let iq = build_item_publish_iq(&service, &item_id, &item);
            send_iq_command(inner, iq).await?;
            let js_master = WaddleVEvent::from_vevent(&item_id, item.master);
            to_js_value(&js_master)
        })
    }

    /// Retract a calendar item from the events node. Used for
    /// "cancel entire series" — removes the master plus any
    /// overrides in one shot.
    pub fn xcal_retract(&self, community_jid: String, item_id: String) -> js_sys::Promise {
        let inner = self.inner.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            if item_id.is_empty() {
                return Err(js_error("item_id must not be empty"));
            }
            let service = service_bare_jid(&community_jid)?;
            let id = format!("xcal-retract-{}", Uuid::new_v4());
            let iq =
                build_pubsub_retract_iq(&id, Some(&service), PUBSUB_NODE_EVENTS, item_id.as_str());
            send_iq_command(inner, iq).await?;
            Ok(JsValue::TRUE)
        })
    }

    /// Publish (or update) this session's RSVP for a calendar event.
    /// `partstat` must be one of "ACCEPTED" | "DECLINED" | "TENTATIVE"
    /// | "NEEDS-ACTION". The chat groups sibling `-rsvp-*` items back
    /// into the master event on the next items fetch.
    pub fn xcal_rsvp(
        &self,
        community_jid: String,
        master_uid: String,
        self_localpart: String,
        self_jid: String,
        partstat: String,
    ) -> js_sys::Promise {
        let inner = self.inner.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let partstat = PartStat::from_str_value(&partstat)
                .ok_or_else(|| js_error(format!("invalid partstat: {partstat}")))?;
            if master_uid.is_empty() {
                return Err(js_error("master_uid must not be empty"));
            }
            if self_localpart.is_empty() {
                return Err(js_error("self_localpart must not be empty"));
            }
            if !self_jid.contains('@') {
                return Err(js_error("self_jid must be a bare JID"));
            }
            let service = service_bare_jid(&community_jid)?;
            let iq =
                build_rsvp_publish_iq(&service, &master_uid, &self_localpart, &self_jid, partstat);
            send_iq_command(inner, iq).await?;
            to_js_value(&WaddleAttendee {
                uri: format!("xmpp:{self_jid}"),
                partstat: partstat.as_str().to_string(),
                role: None,
                rsvp: None,
            })
        })
    }
}

use serde::Serialize;
use serde_json::Value;
use waddle_xmpp_client_ffi::WaddleClientEvent;

use crate::error::RuntimeError;
use crate::generated::Output;

/// Kept typed until poll serializes at the C ABI I/O boundary.
pub(crate) enum Outbound {
    Result {
        request_id: u64,
        result: Result<Box<Output>, RuntimeError>,
    },
    Event(Box<WaddleClientEvent>),
    SessionLifecycle,
    MdsDisplayed(Vec<waddle_xmpp_client_ffi::WaddleMdsDisplayedEntry>),
    Overflow,
}

pub(crate) struct Envelope {
    pub(crate) generation: u64,
    pub(crate) sequence: u64,
    pub(crate) outbound: Outbound,
}

impl Serialize for Envelope {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut out = serializer.serialize_map(None)?;
        out.serialize_entry("generation", &self.generation)?;
        out.serialize_entry("sequence", &self.sequence)?;
        match &self.outbound {
            Outbound::Result { request_id, result } => {
                out.serialize_entry("kind", "result")?;
                out.serialize_entry("request_id", request_id)?;
                out.serialize_entry("ok", &result.is_ok())?;
                match result {
                    Ok(value) => out.serialize_entry("data", value)?,
                    Err(error) => out.serialize_entry("error", error)?,
                }
            }
            event => {
                out.serialize_entry("kind", "event")?;
                let value = event_view(event).map_err(serde::ser::Error::custom)?;
                out.serialize_entry("event", &value)?;
            }
        }
        out.end()
    }
}

#[derive(Serialize)]
struct EventView {
    #[serde(rename = "type")]
    kind: &'static str,
    data: Value,
}

fn event_view(event: &Outbound) -> Result<EventView, serde_json::Error> {
    let (kind, data) = match event {
        Outbound::Event(event) => client_event_view(event)?,
        Outbound::SessionLifecycle => ("sessionLifecycle", serde_json::json!({"mode": "fresh"})),
        Outbound::MdsDisplayed(entries) => ("mdsDisplayed", serde_json::to_value(entries)?),
        Outbound::Overflow => ("error", serde_json::to_value(RuntimeError::overflow())?),
        Outbound::Result { .. } => (
            "error",
            serde_json::to_value(RuntimeError::invalid("Invalid event envelope"))?,
        ),
    };
    Ok(EventView { kind, data })
}

fn client_event_view(
    event: &WaddleClientEvent,
) -> Result<(&'static str, Value), serde_json::Error> {
    let pair = match event {
        WaddleClientEvent::Connected => ("connected", Value::Null),
        WaddleClientEvent::Disconnected => ("disconnected", Value::Null),
        WaddleClientEvent::Message { message } => ("message", serde_json::to_value(message)?),
        WaddleClientEvent::Presence { presence } => ("presence", serde_json::to_value(presence)?),
        WaddleClientEvent::MamResult { message } => ("mamResult", serde_json::to_value(message)?),
        WaddleClientEvent::DeliveryAcked { stanza_id } => {
            ("deliveryAcked", serde_json::json!({"stanza_id": stanza_id}))
        }
        WaddleClientEvent::DeliveryFailed { stanza_id } => (
            "deliveryFailed",
            serde_json::json!({"stanza_id": stanza_id}),
        ),
        WaddleClientEvent::MessageRejected {
            stanza_id,
            from,
            to,
        } => (
            "messageRejected",
            serde_json::json!({"stanza_id": stanza_id.as_str(), "from": from, "to": to}),
        ),
        WaddleClientEvent::AvatarChanged { jid, avatar_id } => (
            "avatarChanged",
            serde_json::json!({"jid": jid, "avatar_id": avatar_id}),
        ),
        WaddleClientEvent::StatusPreferenceChanged { preference } => {
            ("statusPreferenceChanged", serde_json::to_value(preference)?)
        }
        WaddleClientEvent::InboxPush { entry } => ("inboxPush", serde_json::to_value(entry)?),
        WaddleClientEvent::Call { event } => ("call", serde_json::to_value(event)?),
        WaddleClientEvent::AuthenticationFailed { condition } => (
            "authenticationFailed",
            serde_json::json!({"condition": condition}),
        ),
        WaddleClientEvent::Error { .. } => (
            "error",
            serde_json::json!({"code": "client_error", "message": "The XMPP operation failed"}),
        ),
    };
    Ok(pair)
}

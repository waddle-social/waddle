use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};
use waddle_xmpp_client::messaging::MessageRejection;

/// Convert typed protocol fields only at the JavaScript callback boundary.
pub(crate) struct JsMessageRejection<'a>(pub(crate) &'a MessageRejection);

impl Serialize for JsMessageRejection<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Error<'a> {
            error_type: &'a str,
            condition: &'a str,
            text: Option<&'a str>,
        }

        let rejection = self.0;
        let condition = minidom::Element::from(rejection.error.defined_condition.clone());
        let error_type = rejection.error.type_.to_string();
        let error = Error {
            error_type: &error_type,
            condition: condition.name(),
            text: rejection
                .error
                .texts
                .get("")
                .or_else(|| rejection.error.texts.values().next())
                .map(String::as_str),
        };
        let mut object = serializer.serialize_struct("MessageRejection", 4)?;
        object.serialize_field("stanza_id", rejection.stanza_id.as_str())?;
        object.serialize_field("from", &rejection.from.to_string())?;
        object.serialize_field("to", &rejection.to.as_ref().map(ToString::to_string))?;
        object.serialize_field("error", &error)?;
        object.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use waddle_xmpp_client::{ClientConfig, ClientEvent, XmppRuntime};

    #[test]
    fn callback_payload_contains_only_rejection_metadata() {
        let config = ClientConfig::new(
            waddle_xmpp_client::ConnectionConfig::new("example.com".parse().unwrap()),
            waddle_xmpp_client::WebSocketConfig::new("wss://example.com/ws".parse().unwrap())
                .unwrap(),
            waddle_xmpp_client::OAuthBearerConfig::new(
                "alice@example.com".parse().unwrap(),
                waddle_xmpp_client::ClientResource::new("web").unwrap(),
                waddle_xmpp_client::AccessToken::new("test"),
            )
            .unwrap(),
        )
        .unwrap();
        let mut runtime = XmppRuntime::new(config).unwrap();
        let stanza = "<message xmlns='jabber:client' type='error' id='m1' from='chat@example.com' to='alice@example.com/web'><body>echo</body><error type='cancel'><service-unavailable xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/><text xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'>No such account</text></error></message>".parse().unwrap();
        let events = runtime.handle_app_stanza(&stanza);
        let [ClientEvent::MessageRejected(rejection)] = events.as_slice() else {
            panic!("expected rejection")
        };
        assert_eq!(
            serde_json::to_value(JsMessageRejection(rejection)).unwrap(),
            serde_json::json!({
                "stanza_id": "m1",
                "from": "chat@example.com",
                "to": "alice@example.com/web",
                "error": {"error_type": "cancel", "condition": "service-unavailable", "text": "No such account"}
            })
        );
    }
}

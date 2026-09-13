//! Typed frozen message evidence, encoded only by the intent storage codec.

use std::hash::{Hash, Hasher};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use xmpp_parsers::message::Message;

use super::EffectIntentCodecError;

/// Immutable message whose equality is the canonical storage representation.
/// Construction validates serialization, so equality and hashing cannot discard
/// invalid payloads. The inner message is deliberately not mutable.
#[derive(Debug, Clone)]
pub struct StoredMessagePayload(Message);

impl StoredMessagePayload {
    pub fn new(message: Message) -> Result<Self, EffectIntentCodecError> {
        crate::parser::message_to_string(&message)
            .map_err(|_| EffectIntentCodecError::MalformedPayload)?;
        Ok(Self(message))
    }

    pub fn message(&self) -> &Message {
        &self.0
    }

    fn canonical(&self) -> Result<String, EffectIntentCodecError> {
        crate::parser::message_to_string(&self.0)
            .map_err(|_| EffectIntentCodecError::MalformedPayload)
    }
}

impl PartialEq for StoredMessagePayload {
    fn eq(&self, other: &Self) -> bool {
        self.canonical() == other.canonical()
    }
}

impl Eq for StoredMessagePayload {}

impl Hash for StoredMessagePayload {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Construction has already validated the immutable message. Mapping the
        // error keeps the hashing implementation total without a panic path.
        self.canonical().map_err(|_| ()).hash(state);
    }
}

impl Serialize for StoredMessagePayload {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.canonical()
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for StoredMessagePayload {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let encoded = String::deserialize(deserializer)?;
        let message =
            crate::parser::message_from_string(&encoded).map_err(serde::de::Error::custom)?;
        Self::new(message).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::hash_map::DefaultHasher;

    use minidom::Element;
    use xmpp_parsers::message::Lang;

    use super::*;

    #[test]
    fn frozen_message_codec_preserves_thread_parent_and_hash() {
        let ns = crate::parser::ns::JABBER_CLIENT;
        let element = Element::builder("message", ns)
            .attr(
                "from".try_into().expect("attribute name"),
                "room@conference.example.test",
            )
            .attr("type".try_into().expect("attribute name"), "groupchat")
            .append(Element::builder("body", ns).append("pin").build())
            .append(
                Element::builder("thread", ns)
                    .attr("parent".try_into().expect("attribute name"), "parent")
                    .append("child")
                    .build(),
            )
            .build();
        let source = crate::parser::element_to_string(&element).expect("encode message");
        let message = crate::parser::message_from_string(&source).expect("decode message");
        let payload = StoredMessagePayload::new(message).expect("freeze message");
        let encoded = serde_json::to_vec(&payload).expect("encode payload");
        let restored: StoredMessagePayload =
            serde_json::from_slice(&encoded).expect("decode payload");
        assert_eq!(restored, payload);
        let hash = |payload: &StoredMessagePayload| {
            let mut state = DefaultHasher::new();
            payload.hash(&mut state);
            state.finish()
        };
        assert_eq!(hash(&payload), hash(&restored));
        let restored_xml: Element = crate::parser::message_to_string(restored.message())
            .expect("encode restored message")
            .parse()
            .expect("parse restored message");
        assert_eq!(
            restored_xml
                .get_child("thread", ns)
                .expect("thread")
                .attr("parent"),
            Some("parent")
        );
        assert_eq!(
            restored
                .message()
                .bodies
                .get(&Lang::default())
                .map(String::as_str),
            Some("pin")
        );
    }
}

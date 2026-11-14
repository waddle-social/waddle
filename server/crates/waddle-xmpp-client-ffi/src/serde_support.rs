//! Optional JSON boundary adapters for protocol types whose native implementations stay unchanged.

pub(crate) mod stanza_id {
    use serde::{Deserialize, Deserializer, Serializer};
    use waddle_xmpp_client::StanzaId;

    pub(crate) fn serialize<S: Serializer>(
        value: &StanzaId,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(value.as_str())
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<StanzaId, D::Error> {
        let value = String::deserialize(deserializer)?;
        StanzaId::try_new(value).map_err(serde::de::Error::custom)
    }
}

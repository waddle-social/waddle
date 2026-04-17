use minidom::Element;
use serde::{Deserialize, Serialize};
use xmpp_parsers::message::Message;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DetectedLink {
    pub url: String,
    pub start_offset: u32,
    pub end_offset: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmbedElement {
    pub element_name: String,
    pub namespace: String,
    #[serde(default)]
    pub attributes: Vec<(String, String)>,
    #[serde(default)]
    pub children: Vec<String>,
}

impl EmbedElement {
    pub fn to_minidom(&self) -> Element {
        let mut builder = Element::builder(self.element_name.as_str(), self.namespace.as_str());
        for (key, value) in &self.attributes {
            builder = builder.attr(key.as_str(), value.as_str());
        }

        for child in &self.children {
            if let Ok(elem) = child.parse::<Element>() {
                builder = builder.append(elem);
            } else {
                builder = builder.append(child.clone());
            }
        }

        builder.build()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FeatureAdvertisement {
    pub namespace: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionInfo {
    pub name: String,
    pub namespace: String,
    pub version: String,
    #[serde(default)]
    pub features: Vec<FeatureAdvertisement>,
}

pub fn message_has_embed_for_namespaces(msg: &Message, namespaces: &[String]) -> bool {
    msg.payloads
        .iter()
        .any(|payload| namespaces.iter().any(|ns| payload.ns() == ns.as_str()))
}

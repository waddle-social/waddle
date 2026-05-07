use super::*;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct XmlAttribute {
    pub namespace: Option<PayloadNamespace>,
    pub local_name: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum XmlNode {
    Element(XmlElement),
    Text(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct XmlElement {
    pub namespace: PayloadNamespace,
    pub local_name: String,
    pub attributes: Vec<XmlAttribute>,
    pub children: Vec<XmlNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionPayload {
    pub namespace: PayloadNamespace,
    pub root: XmlElement,
}

impl ExtensionPayload {
    pub fn new(namespace: PayloadNamespace, root: XmlElement) -> Result<Self, FrameworkTypeError> {
        if root.namespace != namespace {
            return Err(FrameworkTypeError::InvalidPayloadNamespace(
                root.namespace.as_str().to_string(),
            ));
        }
        root.validate(0)?;
        let payload = Self { namespace, root };
        if payload.serialized_len() > MAX_XML_SERIALIZED_BYTES {
            return Err(FrameworkTypeError::XmlLimitExceeded {
                limit: "serialized payload bytes",
            });
        }
        Ok(payload)
    }

    pub fn to_minidom(&self) -> Element {
        self.root.to_minidom()
    }

    /// Returns true when this payload is the framework-defined
    /// `<extension-item>` PubSub envelope. The host accepts these from any
    /// extension that has been granted `PubSubPublish`, without requiring the
    /// manifest to declare the framework namespace.
    pub fn is_framework_item(&self) -> bool {
        self.namespace.is_framework()
            && self.root.namespace.is_framework()
            && self.root.local_name == EXTENSION_ITEM_LOCAL_NAME
    }

    fn serialized_len(&self) -> usize {
        self.root.serialized_len()
    }
}

impl XmlElement {
    pub fn new(
        namespace: PayloadNamespace,
        local_name: impl Into<String>,
        attributes: Vec<XmlAttribute>,
        children: Vec<XmlNode>,
    ) -> Result<Self, FrameworkTypeError> {
        let element = Self {
            namespace,
            local_name: validate_xml_local_name(local_name.into())?,
            attributes,
            children,
        };
        element.validate(0)?;
        Ok(element)
    }

    fn validate(&self, depth: usize) -> Result<(), FrameworkTypeError> {
        if depth > MAX_XML_DEPTH {
            return Err(FrameworkTypeError::XmlLimitExceeded { limit: "depth" });
        }
        validate_xml_local_name(self.local_name.clone())?;
        if self.attributes.len() > MAX_XML_ATTRIBUTES {
            return Err(FrameworkTypeError::XmlLimitExceeded {
                limit: "attributes per element",
            });
        }
        if self.children.len() > MAX_XML_CHILDREN {
            return Err(FrameworkTypeError::XmlLimitExceeded {
                limit: "children per element",
            });
        }
        let mut seen = std::collections::HashSet::new();
        for attr in &self.attributes {
            validate_xml_local_name(attr.local_name.clone())?;
            if attr.namespace.is_some() {
                return Err(FrameworkTypeError::NamespacedXmlAttributeUnsupported);
            }
            let key = (
                attr.namespace.as_ref().map(|ns| ns.as_str().to_string()),
                attr.local_name.clone(),
            );
            if !seen.insert(key.clone()) {
                return Err(FrameworkTypeError::DuplicateXmlAttribute {
                    namespace: key.0,
                    local_name: key.1,
                });
            }
        }
        for child in &self.children {
            match child {
                XmlNode::Element(element) => element.validate(depth + 1)?,
                XmlNode::Text(text) if text.len() > MAX_XML_TEXT_BYTES => {
                    return Err(FrameworkTypeError::XmlLimitExceeded {
                        limit: "text node bytes",
                    });
                }
                XmlNode::Text(_) => {}
            }
        }
        Ok(())
    }

    fn to_minidom(&self) -> Element {
        let mut builder = Element::builder(self.local_name.as_str(), self.namespace.as_str());
        for attr in &self.attributes {
            debug_assert!(attr.namespace.is_none());
            builder = builder.attr(attr.local_name.as_str(), attr.value.as_str());
        }
        for child in &self.children {
            builder = match child {
                XmlNode::Element(element) => builder.append(element.to_minidom()),
                XmlNode::Text(text) => builder.append(text.clone()),
            };
        }
        builder.build()
    }

    fn serialized_len(&self) -> usize {
        self.local_name.len()
            + self.namespace.as_str().len()
            + self
                .attributes
                .iter()
                .map(|attr| {
                    attr.local_name.len()
                        + attr.value.len()
                        + attr
                            .namespace
                            .as_ref()
                            .map(|ns| ns.as_str().len())
                            .unwrap_or(0)
                })
                .sum::<usize>()
            + self
                .children
                .iter()
                .map(|child| match child {
                    XmlNode::Element(element) => element.serialized_len(),
                    XmlNode::Text(text) => text.len(),
                })
                .sum::<usize>()
    }
}

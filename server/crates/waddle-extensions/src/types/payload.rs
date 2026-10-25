//! Bounded extension XML trees and custom payload serialization.

use super::{
    validate_xml_local_name, Deserialize, Element, FrameworkTypeError, PayloadNamespace, Serialize,
    EXTENSION_ITEM_LOCAL_NAME, MAX_XML_ATTRIBUTES, MAX_XML_CHILDREN, MAX_XML_DEPTH,
    MAX_XML_SERIALIZED_BYTES, MAX_XML_TEXT_BYTES,
};
use std::collections::HashSet;

/// Unprefixed XML attribute name and text; namespaced attributes are unsupported by the serializer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct XmlAttribute {
    /// XML namespace URI of the element or attribute.
    pub namespace: Option<PayloadNamespace>,
    /// Local XML name without a namespace prefix.
    pub local_name: String,
    /// Text value written to the unprefixed XML attribute.
    pub value: String,
}

/// Child element or text node in the bounded extension XML tree.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum XmlNode {
    /// Nested namespace-qualified element.
    Element(XmlElement),
    /// Text content preserved as a child node.
    Text(String),
}

/// Namespace-qualified extension XML element with ordered children and unprefixed attributes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct XmlElement {
    /// XML namespace URI of the element or attribute.
    pub namespace: PayloadNamespace,
    /// Local XML name without a namespace prefix.
    pub local_name: String,
    /// Unprefixed XML attributes attached to this element.
    pub attributes: Vec<XmlAttribute>,
    /// Element and text children retained in document order.
    pub children: Vec<XmlNode>,
}

/// Validated extension XML root and matching advertised namespace.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionPayload {
    /// XML namespace URI of the element or attribute.
    pub namespace: PayloadNamespace,
    /// Root XML element whose namespace matches the payload namespace.
    pub root: XmlElement,
}

impl ExtensionPayload {
    /// Validate the root namespace and bounded XML tree, then enforce the content-size budget.
    /// Limits are depth 16 below the root, 64 attributes and 256 children per
    /// element, 16 KiB per text node, and a 64 KiB aggregate sum of names,
    /// namespaces, attributes, and text bytes. XML serialization overhead is
    /// not included in that aggregate sum.
    ///
    /// # Errors
    ///
    /// Returns a framework error when the advertised namespace differs from the root, any
    /// XML tree validation fails, or the aggregate content-size budget exceeds 64 KiB.
    pub fn try_new(
        namespace: PayloadNamespace,
        root: XmlElement,
    ) -> Result<Self, FrameworkTypeError> {
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

    /// Serialize this extension model into the framework's typed XML element tree.
    ///
    /// # Panics
    /// Panics if public attribute fields were manually constructed or mutated to
    /// contain invalid XML local names. In debug builds, namespaced attributes
    /// also panic. Values accepted by the validated constructors satisfy these
    /// serialization preconditions.
    #[must_use]
    pub fn to_minidom(&self) -> Element {
        self.root.to_minidom()
    }

    /// Parse a `minidom::Element` back into an `ExtensionPayload`.
    ///
    /// Used by the host's PubSub bridge to expose stored application XML.
    /// The element's namespace becomes the payload namespace; child elements
    /// keep their own namespace and undergo the same namespace/name checks.
    ///
    /// # Errors
    ///
    /// Returns a framework error for an invalid or official element namespace, invalid
    /// local names, duplicate attributes after namespace removal, tree limits, or the
    /// aggregate payload-size budget.
    pub fn from_minidom(element: &Element) -> Result<Self, FrameworkTypeError> {
        let root = XmlElement::from_minidom(element)?;
        Self::try_new(root.namespace.clone(), root)
    }

    /// Returns true when this payload is the framework-defined
    /// `<extension-item>` PubSub envelope. The host accepts these from any
    /// extension that has been granted `PubSubPublish`, without requiring the
    /// manifest to declare the framework namespace.
    #[must_use]
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
    /// Validate an XML element name, attributes, child trees, and per-node size/depth limits.
    ///
    /// # Errors
    ///
    /// Returns a framework error for unsupported local names, namespaced or duplicate
    /// attributes, or excessive depth, attribute/child counts, or text-node bytes.
    pub fn try_new(
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
        let mut seen = HashSet::new();
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
            // Extension payload attribute names are validated at
            // `XmlAttribute::new` time; they are therefore well-formed
            // NcNames and the conversion below is infallible.
            let name: minidom::rxml::NcName = attr
                .local_name
                .as_str()
                .try_into()
                .expect("XmlAttribute local_name is a validated NcName");
            builder = builder.attr(name, attr.value.as_str());
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

    /// Parse a `minidom::Element` into a typed tree, preserving child namespaces
    /// and validating names and tree limits through `XmlElement::try_new`.
    /// Attribute namespace information is discarded by this conversion.
    ///
    /// # Errors
    ///
    /// Returns a framework error for invalid or official element namespaces, unsupported
    /// local names, duplicate attributes after namespace removal, or tree limits.
    pub fn from_minidom(element: &Element) -> Result<Self, FrameworkTypeError> {
        let namespace = PayloadNamespace::try_new(element.ns())?;
        let attributes = element
            .attrs()
            .iter()
            .map(|((_ns, name), value)| XmlAttribute {
                namespace: None,
                local_name: name.as_str().to_string(),
                value: value.clone(),
            })
            .collect::<Vec<_>>();
        let mut children = Vec::with_capacity(element.nodes().count());
        for node in element.nodes() {
            match node {
                minidom::Node::Element(child) => {
                    children.push(XmlNode::Element(XmlElement::from_minidom(child)?));
                }
                minidom::Node::Text(text) => {
                    if !text.is_empty() {
                        children.push(XmlNode::Text(text.clone()));
                    }
                }
            }
        }
        Self::try_new(namespace, element.name().to_string(), attributes, children)
    }
}

//! XEP-0049: Private XML Storage
//!
//! Allows users to store arbitrary XML data on the server, keyed by namespace.
//! This is a simple key-value store per user.

use minidom::Element;
use xmpp_parsers::iq::Iq;

/// Namespace for private XML storage.
pub const NS_PRIVATE: &str = "jabber:iq:private";

/// Key for private storage: element name + namespace combination.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PrivateStorageKey {
    /// The element name
    pub element_name: String,
    /// The namespace
    pub namespace: String,
}

impl PrivateStorageKey {
    fn from_element(element: &Element) -> Self {
        Self {
            element_name: element.name().to_string(),
            namespace: element.ns().to_string(),
        }
    }
}

/// Typed arbitrary XML payload stored by XEP-0049.
#[derive(Debug, Clone)]
pub struct PrivateStorageValue {
    /// The first element name + shared namespace storage key.
    pub key: PrivateStorageKey,
    /// The arbitrary XML fragment elements in their original order.
    pub elements: Vec<Element>,
}

impl PrivateStorageValue {
    /// Build a typed storage value from an XML element.
    pub fn from_element(element: Element) -> Result<Self, PrivateStorageError> {
        Self::from_elements(vec![element])
    }

    /// Build a typed storage value from same-namespace XML elements.
    pub fn from_elements(elements: Vec<Element>) -> Result<Self, PrivateStorageError> {
        let first = elements
            .first()
            .ok_or(PrivateStorageError::MissingPayload)?;
        let key = PrivateStorageKey::from_element(first);
        if key.namespace.is_empty() {
            return Err(PrivateStorageError::MissingNamespace);
        }
        if elements
            .iter()
            .any(|element| element.ns() != key.namespace.as_str())
        {
            return Err(PrivateStorageError::MultipleNamespaces);
        }
        Ok(Self { key, elements })
    }

    /// Serialize only at persistence or transport boundaries.
    pub fn to_xml_string(&self) -> String {
        if let [element] = self.elements.as_slice() {
            return String::from(element);
        }

        let mut builder = Element::builder("query", NS_PRIVATE);
        for element in &self.elements {
            builder = builder.append(element.clone());
        }
        String::from(&builder.build())
    }
}

/// XEP-0049 query parsing/storage value errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivateStorageError {
    /// The IQ stanza is not the expected get or set shape.
    UnexpectedIqType,
    /// The payload is not `<query xmlns='jabber:iq:private'/>`.
    UnexpectedQuery,
    /// The query is missing the required private child payload.
    MissingPayload,
    /// The private child payload is not scoped by its own namespace.
    MissingNamespace,
    /// The request attempted to address more than one private payload.
    MultiplePayloads,
    /// A multi-payload SET used more than one child namespace.
    MultipleNamespaces,
    /// Stored XML could not be parsed back into a typed element.
    MalformedStoredXml,
}

/// Check if an IQ is a private XML storage query (XEP-0049).
pub fn is_private_storage_query(iq: &Iq) -> bool {
    match iq {
        Iq::Get { payload: elem, .. } | Iq::Set { payload: elem, .. } => {
            elem.name() == "query" && elem.ns() == NS_PRIVATE
        }
        _ => false,
    }
}

/// Parse a private storage GET request.
///
/// Returns the element name + namespace of the child element being requested.
pub fn parse_private_storage_get(iq: &Iq) -> Result<PrivateStorageKey, PrivateStorageError> {
    let Iq::Get { payload: elem, .. } = &iq else {
        return Err(PrivateStorageError::UnexpectedIqType);
    };

    ensure_private_query(elem)?;
    let key = PrivateStorageKey::from_element(exactly_one_child(elem)?);
    if key.namespace.is_empty() {
        return Err(PrivateStorageError::MissingNamespace);
    }
    Ok(key)
}

/// Parse a private storage SET request.
///
/// Returns the typed arbitrary XML content to store.
pub fn parse_private_storage_set(iq: &Iq) -> Result<PrivateStorageValue, PrivateStorageError> {
    let Iq::Set { payload: elem, .. } = &iq else {
        return Err(PrivateStorageError::UnexpectedIqType);
    };

    ensure_private_query(elem)?;
    PrivateStorageValue::from_elements(elem.children().cloned().collect())
}

/// Parse serialized storage data back into a typed private XML value.
pub fn parse_stored_private_storage_value(
    xml_content: &str,
) -> Result<PrivateStorageValue, PrivateStorageError> {
    let element = xml_content
        .parse::<Element>()
        .map_err(|_| PrivateStorageError::MalformedStoredXml)?;

    if element.name() == "query" && element.ns() == NS_PRIVATE {
        return PrivateStorageValue::from_elements(element.children().cloned().collect());
    }

    PrivateStorageValue::from_element(element)
}

/// Build a private storage result IQ (response to GET).
pub fn build_private_storage_result(
    original_iq: &Iq,
    value: Option<&PrivateStorageValue>,
    key: &PrivateStorageKey,
) -> Iq {
    let mut query_builder = Element::builder("query", NS_PRIVATE);

    if let Some(value) = value {
        for element in &value.elements {
            query_builder = query_builder.append(element.clone());
        }
    } else {
        // No stored data - return empty element with the namespace
        let empty = Element::builder(&key.element_name, &key.namespace).build();
        query_builder = query_builder.append(empty);
    }

    let query = query_builder.build();

    Iq::Result {
        from: original_iq.to().cloned(),
        to: original_iq.from().cloned(),
        id: original_iq.id().to_string(),
        payload: Some(query),
    }
}

/// Build a private storage success response (response to SET).
pub fn build_private_storage_success(original_iq: &Iq) -> Iq {
    Iq::Result {
        from: original_iq.to().cloned(),
        to: original_iq.from().cloned(),
        id: original_iq.id().to_string(),
        payload: None,
    }
}

fn ensure_private_query(element: &Element) -> Result<(), PrivateStorageError> {
    if element.name() == "query" && element.ns() == NS_PRIVATE {
        Ok(())
    } else {
        Err(PrivateStorageError::UnexpectedQuery)
    }
}

fn exactly_one_child(element: &Element) -> Result<&Element, PrivateStorageError> {
    let mut children = element.children();
    let child = children.next().ok_or(PrivateStorageError::MissingPayload)?;
    if children.next().is_some() {
        return Err(PrivateStorageError::MultiplePayloads);
    }
    Ok(child)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_private_storage_query_get() {
        let child = Element::builder("storage", "storage:bookmarks").build();
        let query = Element::builder("query", NS_PRIVATE).append(child).build();
        let iq = Iq::Get {
            from: None,
            to: None,
            id: "test-1".to_string(),
            payload: query,
        };
        assert!(is_private_storage_query(&iq));
    }

    #[test]
    fn test_is_private_storage_query_set() {
        let child = Element::builder("storage", "storage:bookmarks").build();
        let query = Element::builder("query", NS_PRIVATE).append(child).build();
        let iq = Iq::Set {
            from: None,
            to: None,
            id: "test-2".to_string(),
            payload: query,
        };
        assert!(is_private_storage_query(&iq));
    }

    #[test]
    fn test_parse_private_storage_get() {
        let child = Element::builder("storage", "storage:bookmarks").build();
        let query = Element::builder("query", NS_PRIVATE).append(child).build();
        let iq = Iq::Get {
            from: None,
            to: None,
            id: "test-3".to_string(),
            payload: query,
        };
        let key = parse_private_storage_get(&iq).unwrap();
        assert_eq!(key.element_name, "storage");
        assert_eq!(key.namespace, "storage:bookmarks");
    }

    #[test]
    fn test_parse_private_storage_set() {
        let child = Element::builder("storage", "storage:bookmarks")
            .append(Element::builder("conference", "storage:bookmarks").build())
            .build();
        let query = Element::builder("query", NS_PRIVATE).append(child).build();
        let iq = Iq::Set {
            from: None,
            to: None,
            id: "test-4".to_string(),
            payload: query,
        };
        let result = parse_private_storage_set(&iq).unwrap();
        assert_eq!(result.key.element_name, "storage");
        assert_eq!(result.key.namespace, "storage:bookmarks");
        assert_eq!(result.elements[0].name(), "storage");
    }
}

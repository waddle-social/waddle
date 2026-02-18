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

/// Check if an IQ is a private XML storage query (XEP-0049).
pub fn is_private_storage_query(iq: &Iq) -> bool {
    match &iq.payload {
        xmpp_parsers::iq::IqType::Get(elem) | xmpp_parsers::iq::IqType::Set(elem) => {
            elem.name() == "query" && elem.ns() == NS_PRIVATE
        }
        _ => false,
    }
}

/// Parse a private storage GET request.
///
/// Returns the namespace of the child element being requested.
pub fn parse_private_storage_get(iq: &Iq) -> Option<PrivateStorageKey> {
    if let xmpp_parsers::iq::IqType::Get(elem) = &iq.payload {
        if elem.name() == "query" && elem.ns() == NS_PRIVATE {
            if let Some(child) = elem.children().next() {
                return Some(PrivateStorageKey {
                    element_name: child.name().to_string(),
                    namespace: child.ns().to_string(),
                });
            }
        }
    }
    None
}

/// Parse a private storage SET request.
///
/// Returns the namespace and the full XML content to store.
pub fn parse_private_storage_set(iq: &Iq) -> Option<(PrivateStorageKey, String)> {
    if let xmpp_parsers::iq::IqType::Set(elem) = &iq.payload {
        if elem.name() == "query" && elem.ns() == NS_PRIVATE {
            if let Some(child) = elem.children().next() {
                let key = PrivateStorageKey {
                    element_name: child.name().to_string(),
                    namespace: child.ns().to_string(),
                };
                let xml_content = String::from(child);
                return Some((key, xml_content));
            }
        }
    }
    None
}

/// Build a private storage result IQ (response to GET).
pub fn build_private_storage_result(
    original_iq: &Iq,
    xml_content: Option<&str>,
    key: &PrivateStorageKey,
) -> Iq {
    let mut query_builder = Element::builder("query", NS_PRIVATE);

    if let Some(content) = xml_content {
        // Try to parse the stored XML back into an element
        if let Ok(elem) = content.parse::<Element>() {
            query_builder = query_builder.append(elem);
        } else {
            // If parsing fails, return empty element with the namespace
            let empty = Element::builder(&key.element_name, &key.namespace).build();
            query_builder = query_builder.append(empty);
        }
    } else {
        // No stored data - return empty element with the namespace
        let empty = Element::builder(&key.element_name, &key.namespace).build();
        query_builder = query_builder.append(empty);
    }

    let query = query_builder.build();

    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: xmpp_parsers::iq::IqType::Result(Some(query)),
    }
}

/// Build a private storage success response (response to SET).
pub fn build_private_storage_success(original_iq: &Iq) -> Iq {
    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: xmpp_parsers::iq::IqType::Result(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_private_storage_query_get() {
        let child = Element::builder("storage", "storage:bookmarks").build();
        let query = Element::builder("query", NS_PRIVATE).append(child).build();
        let iq = Iq {
            from: None,
            to: None,
            id: "test-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Get(query),
        };
        assert!(is_private_storage_query(&iq));
    }

    #[test]
    fn test_is_private_storage_query_set() {
        let child = Element::builder("storage", "storage:bookmarks").build();
        let query = Element::builder("query", NS_PRIVATE).append(child).build();
        let iq = Iq {
            from: None,
            to: None,
            id: "test-2".to_string(),
            payload: xmpp_parsers::iq::IqType::Set(query),
        };
        assert!(is_private_storage_query(&iq));
    }

    #[test]
    fn test_parse_private_storage_get() {
        let child = Element::builder("storage", "storage:bookmarks").build();
        let query = Element::builder("query", NS_PRIVATE).append(child).build();
        let iq = Iq {
            from: None,
            to: None,
            id: "test-3".to_string(),
            payload: xmpp_parsers::iq::IqType::Get(query),
        };
        let key = parse_private_storage_get(&iq);
        assert!(key.is_some());
        let key = key.unwrap();
        assert_eq!(key.element_name, "storage");
        assert_eq!(key.namespace, "storage:bookmarks");
    }

    #[test]
    fn test_parse_private_storage_set() {
        let child = Element::builder("storage", "storage:bookmarks")
            .append(Element::builder("conference", "storage:bookmarks").build())
            .build();
        let query = Element::builder("query", NS_PRIVATE).append(child).build();
        let iq = Iq {
            from: None,
            to: None,
            id: "test-4".to_string(),
            payload: xmpp_parsers::iq::IqType::Set(query),
        };
        let result = parse_private_storage_set(&iq);
        assert!(result.is_some());
        let (key, content) = result.unwrap();
        assert_eq!(key.element_name, "storage");
        assert_eq!(key.namespace, "storage:bookmarks");
        assert!(content.contains("storage:bookmarks"));
    }
}

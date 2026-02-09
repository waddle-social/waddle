//! XEP-0191: Blocking Command
//!
//! Implements the XMPP blocking command for managing user blocklists.
//! This extension allows users to block communications from specific JIDs.
//!
//! ## Overview
//!
//! The Blocking Command extension provides:
//! - Retrieving the current blocklist (IQ get with blocklist element)
//! - Adding JIDs to the blocklist (IQ set with block element)
//! - Removing JIDs from the blocklist (IQ set with unblock element)
//! - Push notifications when the blocklist changes
//!
//! ## XML Format
//!
//! ```xml
//! <!-- Get blocklist -->
//! <iq type='get' id='blocklist1'>
//!   <blocklist xmlns='urn:xmpp:blocking'/>
//! </iq>
//!
//! <!-- Blocklist response -->
//! <iq type='result' id='blocklist1'>
//!   <blocklist xmlns='urn:xmpp:blocking'>
//!     <item jid='romeo@montague.net'/>
//!     <item jid='iago@shakespeare.lit'/>
//!   </blocklist>
//! </iq>
//!
//! <!-- Block a JID -->
//! <iq type='set' id='block1'>
//!   <block xmlns='urn:xmpp:blocking'>
//!     <item jid='romeo@montague.net'/>
//!   </block>
//! </iq>
//!
//! <!-- Unblock a JID -->
//! <iq type='set' id='unblock1'>
//!   <unblock xmlns='urn:xmpp:blocking'>
//!     <item jid='romeo@montague.net'/>
//!   </unblock>
//! </iq>
//!
//! <!-- Unblock all JIDs -->
//! <iq type='set' id='unblock2'>
//!   <unblock xmlns='urn:xmpp:blocking'/>
//! </iq>
//! ```

use minidom::Element;
use tracing::debug;
use xmpp_parsers::iq::Iq;

/// Namespace for XEP-0191 Blocking Command.
pub const NS_BLOCKING: &str = "urn:xmpp:blocking";

/// Request type for blocking operations.
#[derive(Debug, Clone)]
pub enum BlockingRequest {
    /// Get the current blocklist
    GetBlocklist,
    /// Block one or more JIDs
    Block(Vec<String>),
    /// Unblock one or more JIDs (empty vec means unblock all)
    Unblock(Vec<String>),
}

/// Errors that can occur during blocking operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockingError {
    /// Bad request (malformed blocking stanza)
    BadRequest(String),
    /// Not authorized to perform this action
    NotAuthorized,
    /// Internal server error
    InternalError(String),
    /// Item not found (e.g., trying to unblock a JID that isn't blocked)
    ItemNotFound(String),
}

impl std::fmt::Display for BlockingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BlockingError::BadRequest(msg) => write!(f, "Bad request: {}", msg),
            BlockingError::NotAuthorized => write!(f, "Not authorized"),
            BlockingError::InternalError(msg) => write!(f, "Internal error: {}", msg),
            BlockingError::ItemNotFound(msg) => write!(f, "Item not found: {}", msg),
        }
    }
}

impl std::error::Error for BlockingError {}

/// Check if an IQ stanza is a blocking query (XEP-0191).
///
/// Returns true for `get` (retrieve blocklist) and `set` (block/unblock) types.
pub fn is_blocking_query(iq: &Iq) -> bool {
    match &iq.payload {
        xmpp_parsers::iq::IqType::Get(elem) => {
            elem.name() == "blocklist" && elem.ns() == NS_BLOCKING
        }
        xmpp_parsers::iq::IqType::Set(elem) => {
            (elem.name() == "block" || elem.name() == "unblock") && elem.ns() == NS_BLOCKING
        }
        _ => false,
    }
}

/// Check if an IQ is a blocklist get request.
pub fn is_blocklist_get(iq: &Iq) -> bool {
    match &iq.payload {
        xmpp_parsers::iq::IqType::Get(elem) => {
            elem.name() == "blocklist" && elem.ns() == NS_BLOCKING
        }
        _ => false,
    }
}

/// Check if an IQ is a block set request.
pub fn is_block_set(iq: &Iq) -> bool {
    match &iq.payload {
        xmpp_parsers::iq::IqType::Set(elem) => elem.name() == "block" && elem.ns() == NS_BLOCKING,
        _ => false,
    }
}

/// Check if an IQ is an unblock set request.
pub fn is_unblock_set(iq: &Iq) -> bool {
    match &iq.payload {
        xmpp_parsers::iq::IqType::Set(elem) => elem.name() == "unblock" && elem.ns() == NS_BLOCKING,
        _ => false,
    }
}

/// Parse a blocking request from an IQ stanza.
pub fn parse_blocking_request(iq: &Iq) -> Result<BlockingRequest, BlockingError> {
    match &iq.payload {
        xmpp_parsers::iq::IqType::Get(elem) => {
            if elem.name() == "blocklist" && elem.ns() == NS_BLOCKING {
                Ok(BlockingRequest::GetBlocklist)
            } else {
                Err(BlockingError::BadRequest(
                    "Expected blocklist element".to_string(),
                ))
            }
        }
        xmpp_parsers::iq::IqType::Set(elem) => {
            if elem.ns() != NS_BLOCKING {
                return Err(BlockingError::BadRequest(
                    "Invalid namespace for blocking request".to_string(),
                ));
            }

            let jids = extract_jids_from_element(elem)?;

            match elem.name() {
                "block" => {
                    if jids.is_empty() {
                        Err(BlockingError::BadRequest(
                            "Block request must contain at least one item".to_string(),
                        ))
                    } else {
                        Ok(BlockingRequest::Block(jids))
                    }
                }
                "unblock" => {
                    // Empty unblock means unblock all
                    Ok(BlockingRequest::Unblock(jids))
                }
                _ => Err(BlockingError::BadRequest(format!(
                    "Unknown blocking element: {}",
                    elem.name()
                ))),
            }
        }
        _ => Err(BlockingError::BadRequest(
            "Expected IQ get or set for blocking".to_string(),
        )),
    }
}

/// Extract JIDs from item children of a blocking element.
fn extract_jids_from_element(elem: &Element) -> Result<Vec<String>, BlockingError> {
    let mut jids = Vec::new();

    for child in elem.children() {
        if child.name() == "item" {
            if let Some(jid) = child.attr("jid") {
                jids.push(jid.to_string());
            } else {
                return Err(BlockingError::BadRequest(
                    "Item element missing jid attribute".to_string(),
                ));
            }
        }
    }

    debug!(count = jids.len(), "Extracted JIDs from blocking element");
    Ok(jids)
}

/// Build a blocklist response IQ.
pub fn build_blocklist_response(original_iq: &Iq, blocked_jids: &[String]) -> Iq {
    let mut blocklist_builder = Element::builder("blocklist", NS_BLOCKING);

    for jid in blocked_jids {
        let item = Element::builder("item", NS_BLOCKING)
            .attr("jid", jid.as_str())
            .build();
        blocklist_builder = blocklist_builder.append(item);
    }

    let blocklist = blocklist_builder.build();

    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: xmpp_parsers::iq::IqType::Result(Some(blocklist)),
    }
}

/// Build a success response for block/unblock operations.
pub fn build_blocking_success(original_iq: &Iq) -> Iq {
    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: xmpp_parsers::iq::IqType::Result(None),
    }
}

/// Build a blocking push notification IQ.
///
/// This is sent to all user resources when the blocklist changes.
pub fn build_block_push(to: &jid::Jid, blocked_jids: &[String]) -> Iq {
    let mut block_builder = Element::builder("block", NS_BLOCKING);

    for jid in blocked_jids {
        let item = Element::builder("item", NS_BLOCKING)
            .attr("jid", jid.as_str())
            .build();
        block_builder = block_builder.append(item);
    }

    let block = block_builder.build();

    Iq {
        from: None,
        to: Some(to.clone()),
        id: format!("push-block-{}", uuid::Uuid::new_v4()),
        payload: xmpp_parsers::iq::IqType::Set(block),
    }
}

/// Build an unblock push notification IQ.
///
/// This is sent to all user resources when JIDs are unblocked.
/// An empty jids list means all JIDs were unblocked.
pub fn build_unblock_push(to: &jid::Jid, unblocked_jids: &[String]) -> Iq {
    let mut unblock_builder = Element::builder("unblock", NS_BLOCKING);

    for jid in unblocked_jids {
        let item = Element::builder("item", NS_BLOCKING)
            .attr("jid", jid.as_str())
            .build();
        unblock_builder = unblock_builder.append(item);
    }

    let unblock = unblock_builder.build();

    Iq {
        from: None,
        to: Some(to.clone()),
        id: format!("push-unblock-{}", uuid::Uuid::new_v4()),
        payload: xmpp_parsers::iq::IqType::Set(unblock),
    }
}

/// Build a blocking error response.
pub fn build_blocking_error(request_id: &str, error: &BlockingError) -> String {
    let (error_type, condition) = match error {
        BlockingError::BadRequest(_) => ("modify", "bad-request"),
        BlockingError::NotAuthorized => ("auth", "not-authorized"),
        BlockingError::InternalError(_) => ("wait", "internal-server-error"),
        BlockingError::ItemNotFound(_) => ("cancel", "item-not-found"),
    };

    let text = match error {
        BlockingError::BadRequest(msg)
        | BlockingError::InternalError(msg)
        | BlockingError::ItemNotFound(msg) => {
            format!(
                "<text xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'>{}</text>",
                escape_xml(msg)
            )
        }
        _ => String::new(),
    };

    format!(
        "<iq type='error' id='{}'>\
            <blocklist xmlns='{}'/>\
            <error type='{}'>\
                <{} xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/>\
                {}\
            </error>\
        </iq>",
        escape_xml(request_id),
        NS_BLOCKING,
        error_type,
        condition,
        text
    )
}

/// Escape XML special characters.
fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_blocking_query_get_blocklist() {
        let blocklist_elem = Element::builder("blocklist", NS_BLOCKING).build();
        let iq = Iq {
            from: None,
            to: None,
            id: "blocklist-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Get(blocklist_elem),
        };

        assert!(is_blocking_query(&iq));
        assert!(is_blocklist_get(&iq));
        assert!(!is_block_set(&iq));
        assert!(!is_unblock_set(&iq));
    }

    #[test]
    fn test_is_blocking_query_block() {
        let block_elem = Element::builder("block", NS_BLOCKING)
            .append(
                Element::builder("item", NS_BLOCKING)
                    .attr("jid", "romeo@montague.net")
                    .build(),
            )
            .build();
        let iq = Iq {
            from: None,
            to: None,
            id: "block-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Set(block_elem),
        };

        assert!(is_blocking_query(&iq));
        assert!(!is_blocklist_get(&iq));
        assert!(is_block_set(&iq));
        assert!(!is_unblock_set(&iq));
    }

    #[test]
    fn test_is_blocking_query_unblock() {
        let unblock_elem = Element::builder("unblock", NS_BLOCKING)
            .append(
                Element::builder("item", NS_BLOCKING)
                    .attr("jid", "romeo@montague.net")
                    .build(),
            )
            .build();
        let iq = Iq {
            from: None,
            to: None,
            id: "unblock-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Set(unblock_elem),
        };

        assert!(is_blocking_query(&iq));
        assert!(!is_blocklist_get(&iq));
        assert!(!is_block_set(&iq));
        assert!(is_unblock_set(&iq));
    }

    #[test]
    fn test_is_not_blocking_query_wrong_ns() {
        let elem = Element::builder("blocklist", "wrong:namespace").build();
        let iq = Iq {
            from: None,
            to: None,
            id: "test-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Get(elem),
        };

        assert!(!is_blocking_query(&iq));
    }

    #[test]
    fn test_parse_blocklist_get() {
        let blocklist_elem = Element::builder("blocklist", NS_BLOCKING).build();
        let iq = Iq {
            from: None,
            to: None,
            id: "blocklist-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Get(blocklist_elem),
        };

        let request = parse_blocking_request(&iq).unwrap();
        assert!(matches!(request, BlockingRequest::GetBlocklist));
    }

    #[test]
    fn test_parse_block_request() {
        let block_elem = Element::builder("block", NS_BLOCKING)
            .append(
                Element::builder("item", NS_BLOCKING)
                    .attr("jid", "romeo@montague.net")
                    .build(),
            )
            .append(
                Element::builder("item", NS_BLOCKING)
                    .attr("jid", "iago@shakespeare.lit")
                    .build(),
            )
            .build();
        let iq = Iq {
            from: None,
            to: None,
            id: "block-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Set(block_elem),
        };

        let request = parse_blocking_request(&iq).unwrap();
        match request {
            BlockingRequest::Block(jids) => {
                assert_eq!(jids.len(), 2);
                assert!(jids.contains(&"romeo@montague.net".to_string()));
                assert!(jids.contains(&"iago@shakespeare.lit".to_string()));
            }
            _ => panic!("Expected Block request"),
        }
    }

    #[test]
    fn test_parse_unblock_request() {
        let unblock_elem = Element::builder("unblock", NS_BLOCKING)
            .append(
                Element::builder("item", NS_BLOCKING)
                    .attr("jid", "romeo@montague.net")
                    .build(),
            )
            .build();
        let iq = Iq {
            from: None,
            to: None,
            id: "unblock-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Set(unblock_elem),
        };

        let request = parse_blocking_request(&iq).unwrap();
        match request {
            BlockingRequest::Unblock(jids) => {
                assert_eq!(jids.len(), 1);
                assert_eq!(jids[0], "romeo@montague.net");
            }
            _ => panic!("Expected Unblock request"),
        }
    }

    #[test]
    fn test_parse_unblock_all_request() {
        let unblock_elem = Element::builder("unblock", NS_BLOCKING).build();
        let iq = Iq {
            from: None,
            to: None,
            id: "unblock-all-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Set(unblock_elem),
        };

        let request = parse_blocking_request(&iq).unwrap();
        match request {
            BlockingRequest::Unblock(jids) => {
                assert!(jids.is_empty());
            }
            _ => panic!("Expected Unblock request"),
        }
    }

    #[test]
    fn test_parse_empty_block_request_error() {
        let block_elem = Element::builder("block", NS_BLOCKING).build();
        let iq = Iq {
            from: None,
            to: None,
            id: "block-empty-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Set(block_elem),
        };

        let result = parse_blocking_request(&iq);
        assert!(result.is_err());
        match result.unwrap_err() {
            BlockingError::BadRequest(msg) => {
                assert!(msg.contains("at least one item"));
            }
            _ => panic!("Expected BadRequest error"),
        }
    }

    #[test]
    fn test_build_blocklist_response() {
        let blocklist_elem = Element::builder("blocklist", NS_BLOCKING).build();
        let original_iq = Iq {
            from: Some("user@example.com".parse().unwrap()),
            to: Some("server.example.com".parse().unwrap()),
            id: "blocklist-get-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Get(blocklist_elem),
        };

        let blocked_jids = vec![
            "romeo@montague.net".to_string(),
            "iago@shakespeare.lit".to_string(),
        ];

        let response = build_blocklist_response(&original_iq, &blocked_jids);

        assert_eq!(response.id, "blocklist-get-1");
        assert!(matches!(
            response.payload,
            xmpp_parsers::iq::IqType::Result(Some(_))
        ));

        if let xmpp_parsers::iq::IqType::Result(Some(elem)) = &response.payload {
            assert_eq!(elem.name(), "blocklist");
            assert_eq!(elem.ns(), NS_BLOCKING);

            let items: Vec<_> = elem.children().collect();
            assert_eq!(items.len(), 2);

            let jids: Vec<_> = items.iter().filter_map(|item| item.attr("jid")).collect();
            assert!(jids.contains(&"romeo@montague.net"));
            assert!(jids.contains(&"iago@shakespeare.lit"));
        } else {
            panic!("Expected Result with blocklist element");
        }
    }

    #[test]
    fn test_build_empty_blocklist_response() {
        let blocklist_elem = Element::builder("blocklist", NS_BLOCKING).build();
        let original_iq = Iq {
            from: Some("user@example.com".parse().unwrap()),
            to: None,
            id: "blocklist-get-2".to_string(),
            payload: xmpp_parsers::iq::IqType::Get(blocklist_elem),
        };

        let response = build_blocklist_response(&original_iq, &[]);

        assert_eq!(response.id, "blocklist-get-2");
        if let xmpp_parsers::iq::IqType::Result(Some(elem)) = &response.payload {
            assert_eq!(elem.name(), "blocklist");
            assert!(elem.children().next().is_none());
        } else {
            panic!("Expected Result with empty blocklist element");
        }
    }

    #[test]
    fn test_build_blocking_success() {
        let block_elem = Element::builder("block", NS_BLOCKING).build();
        let original_iq = Iq {
            from: Some("user@example.com".parse().unwrap()),
            to: None,
            id: "block-set-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Set(block_elem),
        };

        let response = build_blocking_success(&original_iq);

        assert_eq!(response.id, "block-set-1");
        assert!(matches!(
            response.payload,
            xmpp_parsers::iq::IqType::Result(None)
        ));
    }

    #[test]
    fn test_build_block_push() {
        let blocked_jids = vec!["romeo@montague.net".to_string()];
        let to: jid::Jid = "user@example.com/resource".parse().expect("valid jid");
        let push = build_block_push(&to, &blocked_jids);

        assert!(push.id.starts_with("push-block-"));
        if let xmpp_parsers::iq::IqType::Set(elem) = &push.payload {
            assert_eq!(elem.name(), "block");
            assert_eq!(elem.ns(), NS_BLOCKING);
            let items: Vec<_> = elem.children().collect();
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].attr("jid"), Some("romeo@montague.net"));
        } else {
            panic!("Expected Set with block element");
        }
    }

    #[test]
    fn test_build_unblock_push() {
        let unblocked_jids = vec!["romeo@montague.net".to_string()];
        let to: jid::Jid = "user@example.com/resource".parse().expect("valid jid");
        let push = build_unblock_push(&to, &unblocked_jids);

        assert!(push.id.starts_with("push-unblock-"));
        if let xmpp_parsers::iq::IqType::Set(elem) = &push.payload {
            assert_eq!(elem.name(), "unblock");
            assert_eq!(elem.ns(), NS_BLOCKING);
            let items: Vec<_> = elem.children().collect();
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].attr("jid"), Some("romeo@montague.net"));
        } else {
            panic!("Expected Set with unblock element");
        }
    }

    #[test]
    fn test_build_blocking_error() {
        let error_response = build_blocking_error(
            "error-1",
            &BlockingError::BadRequest("Invalid JID".to_string()),
        );

        assert!(error_response.contains("type='error'"));
        assert!(error_response.contains("id='error-1'"));
        assert!(error_response.contains("<bad-request"));
        assert!(error_response.contains("Invalid JID"));
    }

    #[test]
    fn test_blocking_error_display() {
        assert_eq!(
            BlockingError::BadRequest("test".to_string()).to_string(),
            "Bad request: test"
        );
        assert_eq!(BlockingError::NotAuthorized.to_string(), "Not authorized");
        assert_eq!(
            BlockingError::InternalError("err".to_string()).to_string(),
            "Internal error: err"
        );
        assert_eq!(
            BlockingError::ItemNotFound("jid".to_string()).to_string(),
            "Item not found: jid"
        );
    }
}

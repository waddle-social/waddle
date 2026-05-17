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

use async_trait::async_trait;
use jid::{BareJid, Jid};
use minidom::Element;
use tracing::debug;
use xmpp_parsers::iq::Iq;

/// Namespace for XEP-0191 Blocking Command.
pub const NS_BLOCKING: &str = "urn:xmpp:blocking";

/// Error returned by [`BlockingStorage`] implementations.
///
/// Wraps the implementation's underlying typed error via [`std::error::Error::source`]
/// so the diagnostic chain remains typed end-to-end (no stringly-typed
/// payload — see the typed-payloads hard rule in `CLAUDE.md`).
/// Implementations construct it via [`Self::new`].
///
/// Callers MUST treat any error as fail-closed for XEP-0191 enforcement:
/// the bind path fails the bind with a stream error
/// (`load_blocklist_for_bind`), and the headless offline-recipient pass
/// skips recipient-side processing entirely
/// (`run_headless_recipient_pass`). Degrading to an empty blocklist
/// would silently disable incoming-block enforcement and risk
/// persisting blocked messages into the recipient's archive / inbox.
#[derive(Debug, thiserror::Error)]
#[error("blocking storage error")]
pub struct BlockingStorageError {
    #[source]
    source: Box<dyn std::error::Error + Send + Sync + 'static>,
}

impl BlockingStorageError {
    /// Wrap an implementation-specific error so the protocol-side
    /// trait can carry it without stringification. The underlying
    /// typed error remains accessible via [`std::error::Error::source`].
    pub fn new<E>(source: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        Self {
            source: Box::new(source),
        }
    }
}

/// Storage contract for the per-user XEP-0191 blocklist.
///
/// Defined here so the sans-I/O message pipeline (`waddle-xmpp::protocol`)
/// can be passed a typed storage handle from the I/O layer
/// (`waddle-server`) without taking a hard dependency on the concrete
/// libSQL-backed implementation. The production impl lives in
/// `waddle-server::db::blocking::DatabaseBlockingStorage`; tests can
/// substitute [`InMemoryBlockingStorage`] or a custom fake.
#[async_trait]
pub trait BlockingStorage: Send + Sync {
    /// Return every blocked bare JID for `user`. Used at bind time and
    /// by the headless offline-recipient pass to seed the per-pass
    /// [`crate::protocol::Blocklist`] snapshot consumed by
    /// [`super::super::protocol::handlers::blocking_filter::BlockingFilterHandler`].
    async fn list_blocked_jids(&self, user: &BareJid)
        -> Result<Vec<BareJid>, BlockingStorageError>;

    /// Return blocked JID entries in their stored XEP-0191 form.
    ///
    /// XEP-0191 block items may be bare JIDs, full JIDs, or domain JIDs.
    /// Existing session snapshots still consume [`Self::list_blocked_jids`]
    /// because that path currently performs bare-JID matching only; policy
    /// gates that must honor full/domain entries use this read.
    async fn list_blocked_jid_entries(
        &self,
        user: &BareJid,
    ) -> Result<Vec<Jid>, BlockingStorageError> {
        Ok(self
            .list_blocked_jids(user)
            .await?
            .into_iter()
            .map(Jid::from)
            .collect())
    }
}

/// In-memory [`BlockingStorage`] implementation for tests.
///
/// Backed by a `Mutex<HashMap<BareJid, Vec<Jid>>>`. Used by the
/// offline-recipient-pass tests in `waddle-server` and any handler-level
/// fixture that wants to seed a blocklist without a real database.
#[derive(Debug, Default)]
pub struct InMemoryBlockingStorage {
    per_user: std::sync::Mutex<std::collections::HashMap<BareJid, Vec<Jid>>>,
}

impl InMemoryBlockingStorage {
    /// Construct an empty storage.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the blocklist for `user` with `entries`.
    pub fn set_blocklist(&self, user: BareJid, entries: Vec<BareJid>) {
        self.set_blocklist_jids(user, entries.into_iter().map(Jid::from).collect());
    }

    /// Replace the blocklist for `user` with full XEP-0191 JID entries.
    pub fn set_blocklist_jids(&self, user: BareJid, entries: Vec<Jid>) {
        let mut guard = self
            .per_user
            .lock()
            .expect("InMemoryBlockingStorage mutex poisoned");
        guard.insert(user, entries);
    }
}

/// Typed sentinel error for [`InMemoryBlockingStorage`] faults. The
/// only failure mode is a poisoned `Mutex<HashMap>`; the
/// [`std::sync::PoisonError`] details are not retained because the
/// poisoned guard is unrecoverable in this fixture and the diagnostic
/// would only carry a generic "lock poisoned" string. Callers
/// (typed-payloads rule) match on the error *type* rather than its
/// payload.
#[derive(Debug, thiserror::Error)]
#[error("in-memory blocking storage mutex poisoned")]
pub struct InMemoryBlockingStorageError;

#[async_trait]
impl BlockingStorage for InMemoryBlockingStorage {
    async fn list_blocked_jids(
        &self,
        user: &BareJid,
    ) -> Result<Vec<BareJid>, BlockingStorageError> {
        let guard = self
            .per_user
            .lock()
            .map_err(|_| BlockingStorageError::new(InMemoryBlockingStorageError))?;
        Ok(guard
            .get(user)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|entry| entry.try_into().ok())
            .collect())
    }

    async fn list_blocked_jid_entries(
        &self,
        user: &BareJid,
    ) -> Result<Vec<Jid>, BlockingStorageError> {
        let guard = self
            .per_user
            .lock()
            .map_err(|_| BlockingStorageError::new(InMemoryBlockingStorageError))?;
        Ok(guard.get(user).cloned().unwrap_or_default())
    }
}

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
mod tests;

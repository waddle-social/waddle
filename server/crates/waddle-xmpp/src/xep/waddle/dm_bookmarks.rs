//! Waddle DM-bookmarks wire shape (`urn:waddle:dm-bookmarks:0`).
//!
//! A Waddle-custom PEP carrier that hosts [XEP-0492] *Chat Notification
//! Settings* for **direct (one-to-one) chats**. It is the DM counterpart
//! to the MUC carrier (XEP-0402 PEP Native Bookmarks): XEP-0402 is
//! conference-only, and no XEP-defined "DM bookmark" exists, so the
//! carrier lives in the project-local `urn:waddle:dm-bookmarks:0`
//! namespace per the CLAUDE.md XEP-conformance hard rule.
//!
//! XEP-0492 §2.1 requires `<notify>` to be *"a child of an element
//! identifying a specific chat by its JID, such as a XEP-0402
//! `<extensions>`."* The *"such as"* admits non-XEP-0402 carriers; this
//! carrier identifies the chat by the **PEP item id** (the contact's
//! bare JID), exactly as XEP-0402 does. The decision and rationale are
//! recorded in `docs/adr/009-dm-notification-carrier.md`; the normative
//! wire reference is `docs/specs/urn-waddle-dm-bookmarks.md`.
//!
//! ## Wire shape
//!
//! Published to the owner's own PEP node `urn:waddle:dm-bookmarks:0`,
//! one item per contact (item id == the contact's bare JID):
//!
//! ```xml
//! <iq type='set' id='dm-notify-1'>
//!   <pubsub xmlns='http://jabber.org/protocol/pubsub'>
//!     <publish node='urn:waddle:dm-bookmarks:0'>
//!       <item id='bob@example.com'>
//!         <dm-bookmark xmlns='urn:waddle:dm-bookmarks:0'>
//!           <notify xmlns='urn:xmpp:notification-settings:1'>
//!             <never/>
//!           </notify>
//!         </dm-bookmark>
//!       </item>
//!     </publish>
//!   </pubsub>
//! </iq>
//! ```
//!
//! `<dm-bookmark>` directly hosts a single official XEP-0492 `<notify>`
//! element. There is **no** `<extensions>` wrapper and **no** native
//! field (a DM has no autojoin / nick / password). The `<notify>`
//! element, its namespace (`urn:xmpp:notification-settings:1`), and its
//! children are byte-identical to official XEP-0492 — Waddle hosts it,
//! it does not fork it; the parse/validate logic is reused verbatim from
//! [`crate::xep::xep0492`].
//!
//! Unknown children, attributes, and non-whitespace text content are
//! rejected — clients that want to extend the shape should bump the
//! namespace.
//!
//! ## Publish contract (server side)
//!
//! [`parse_dm_bookmark`] is the publish-time validator: it parses the
//! item id as a bare JID (localpart required), pins the payload root,
//! and validates the hosted `<notify>` via the shared
//! [`crate::xep::xep0492::validate_notify_element`]. The DM node's
//! privacy defaults (`access_model = whitelist` +
//! `send_last_published_item = never`) are pinned in
//! `waddle_xmpp_core::pubsub::NodeConfig::waddle_dm_bookmarks_defaults`,
//! so the per-contact overrides are not broadcast to roster contacts.

use jid::BareJid;
use minidom::Element;
use thiserror::Error;

use crate::xep::xep0492::{is_notify_element, validate_notify_element, NotificationSettingsError};

/// Waddle DM-bookmarks namespace (also the PEP node name, per XEP-0163
/// single-payload-namespace convention).
///
/// Pinned equal to
/// `waddle_xmpp_core::pubsub::PEP_NODE_WADDLE_DM_BOOKMARKS` so
/// `NodeConfig::pep_for_node` in the core crate can apply the
/// `whitelist + send-last-published=never` privacy defaults without
/// pulling `waddle-xmpp` into `waddle-xmpp-core`. The pin is exercised
/// by `pep_node_waddle_dm_bookmarks_constant_matches_core` below.
pub const NS_WADDLE_DM_BOOKMARKS_V0: &str = "urn:waddle:dm-bookmarks:0";

/// PEP node name for DM notification settings. By XEP-0402 convention
/// (mirrored here) the node name equals the payload namespace.
pub const PEP_NODE_WADDLE_DM_BOOKMARKS: &str = NS_WADDLE_DM_BOOKMARKS_V0;

const ELEMENT_DM_BOOKMARK: &str = "dm-bookmark";

/// Check if a PubSub node name is the DM-bookmarks node.
pub fn is_dm_bookmarks_node(node: &str) -> bool {
    node == PEP_NODE_WADDLE_DM_BOOKMARKS
}

/// A parsed DM bookmark: a contact JID plus an optional XEP-0492
/// `<notify>` override.
///
/// `jid` comes from the PEP item id (the contact's bare JID), never
/// from within the payload — mirroring XEP-0402's item-id-as-JID
/// convention. `notify` is the cloned official XEP-0492 `<notify>`
/// child, validated at parse time; `None` means the payload carried no
/// `<notify>` (i.e. "no override" — equivalent to the §3 default).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmBookmark {
    /// Bare JID of the direct-chat contact (from the PEP item id).
    pub jid: BareJid,
    /// The hosted XEP-0492 `<notify>` element, cloned verbatim. `None`
    /// when the `<dm-bookmark>` carried no `<notify>` child.
    pub notify: Option<Element>,
}

/// Errors raised while parsing / validating a `<dm-bookmark>` payload.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum DmBookmarkError {
    /// The PEP item id is not a bare JID with a localpart.
    #[error("invalid DM-bookmark item id (must be a bare JID with a localpart): {0}")]
    InvalidJid(String),
    /// The payload root is not `<dm-bookmark xmlns='urn:waddle:dm-bookmarks:0'>`.
    #[error(
        "expected <{ELEMENT_DM_BOOKMARK} xmlns='{NS_WADDLE_DM_BOOKMARKS_V0}'> root, \
         got <{name} xmlns='{ns}'>"
    )]
    WrongRoot { name: String, ns: String },
    /// A child other than the XEP-0492 `<notify>` appeared in `<dm-bookmark>`.
    #[error("unknown child element <{0}> in <dm-bookmark>")]
    UnknownChild(String),
    /// More than one XEP-0492 `<notify>` child is present.
    #[error("more than one <notify> child in <dm-bookmark>")]
    MultipleNotify,
    /// An attribute appeared on `<dm-bookmark>` (none are defined).
    #[error("unexpected attribute '{0}' on <dm-bookmark>")]
    UnexpectedAttribute(String),
    /// A namespaced attribute appeared on `<dm-bookmark>`.
    #[error("namespaced attribute '{0}' is not allowed on <dm-bookmark>")]
    NamespacedAttribute(String),
    /// Non-whitespace text content appeared in `<dm-bookmark>`.
    #[error("unexpected text content in <dm-bookmark>")]
    UnexpectedTextContent,
    /// The hosted `<notify>` violates the XEP-0492 wire shape.
    #[error("invalid hosted XEP-0492 <notify>: {0}")]
    InvalidNotify(#[from] NotificationSettingsError),
}

/// Strictly parse and validate a `<dm-bookmark>` payload.
///
/// This is also the publish-time validator. It:
///
/// * parses `item_id` as a [`BareJid`] and requires a localpart
///   (`localpart@domain`) — a domain-only or empty id is rejected;
/// * pins the payload root to
///   `<dm-bookmark xmlns='urn:waddle:dm-bookmarks:0'>`;
/// * rejects ANY attribute on `<dm-bookmark>` (none are defined),
///   distinguishing namespaced attributes from bare ones;
/// * rejects non-whitespace text content;
/// * permits exactly one child — the official XEP-0492 `<notify>` —
///   validated via [`crate::xep::xep0492::validate_notify_element`];
///   2+ notify children are [`DmBookmarkError::MultipleNotify`], any
///   other child is [`DmBookmarkError::UnknownChild`].
pub fn parse_dm_bookmark(item_id: &str, payload: &Element) -> Result<DmBookmark, DmBookmarkError> {
    let jid: BareJid = item_id
        .parse()
        .map_err(|_| DmBookmarkError::InvalidJid(item_id.to_string()))?;
    if jid.node().is_none() {
        return Err(DmBookmarkError::InvalidJid(item_id.to_string()));
    }

    if payload.name() != ELEMENT_DM_BOOKMARK || payload.ns() != NS_WADDLE_DM_BOOKMARKS_V0 {
        return Err(DmBookmarkError::WrongRoot {
            name: payload.name().to_string(),
            ns: payload.ns().to_string(),
        });
    }

    reject_unknown_attrs(payload)?;
    reject_text_content(payload)?;

    let mut notify: Option<Element> = None;
    for child in payload.children() {
        if is_notify_element(child) {
            if notify.is_some() {
                return Err(DmBookmarkError::MultipleNotify);
            }
            validate_notify_element(child)?;
            notify = Some(child.clone());
        } else {
            return Err(DmBookmarkError::UnknownChild(child.name().to_string()));
        }
    }

    Ok(DmBookmark { jid, notify })
}

/// Build a `<dm-bookmark xmlns='urn:waddle:dm-bookmarks:0'>` element
/// directly hosting the supplied XEP-0492 `<notify>` (cloned in).
///
/// The caller owns producing a conformant `<notify>` (e.g. via
/// [`crate::xep::xep0492::build_notify_element`]); this builder only
/// wraps it in the carrier root.
pub fn build_dm_bookmark_element(notify: &Element) -> Element {
    Element::builder(ELEMENT_DM_BOOKMARK, NS_WADDLE_DM_BOOKMARKS_V0)
        .append(notify.clone())
        .build()
}

/// Reject any attribute on `<dm-bookmark>` — none are defined by the
/// wire contract. Namespaced attributes are reported separately so a
/// client can't sneak `foo:x='…'` past the gate by riding a different
/// namespace. Mirrors the DND strict-parser `reject_unknown_attrs`
/// pattern with an empty known-attribute set.
fn reject_unknown_attrs(element: &Element) -> Result<(), DmBookmarkError> {
    // No attribute is part of the `<dm-bookmark>` contract, so the
    // presence of ANY attribute is an error: the first one decides the
    // result (a non-empty namespace ⇒ NamespacedAttribute, otherwise
    // UnexpectedAttribute).
    if let Some(((ns, name), _value)) = element.attrs().iter().next() {
        let attr_name = name.as_str();
        if !ns.as_str().is_empty() {
            return Err(DmBookmarkError::NamespacedAttribute(attr_name.to_string()));
        }
        return Err(DmBookmarkError::UnexpectedAttribute(attr_name.to_string()));
    }
    Ok(())
}

/// Reject non-whitespace text content. Text like
/// `<dm-bookmark>oops<notify/></dm-bookmark>` slips past `children()`
/// (which iterates element children only) and would otherwise be
/// silently persisted into `pubsub_items` as raw XML. Pure-whitespace
/// (pretty-printing) text is accepted.
fn reject_text_content(element: &Element) -> Result<(), DmBookmarkError> {
    if element.text().trim().is_empty() {
        Ok(())
    } else {
        Err(DmBookmarkError::UnexpectedTextContent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ITEM_ID: &str = "bob@example.com";

    fn dm_bookmark_xml(inner: &str) -> Element {
        let raw = format!("<dm-bookmark xmlns='{NS_WADDLE_DM_BOOKMARKS_V0}'>{inner}</dm-bookmark>");
        raw.parse().expect("test fixture must be valid XML")
    }

    #[test]
    fn parse_minimal_never_override() {
        let payload =
            dm_bookmark_xml("<notify xmlns='urn:xmpp:notification-settings:1'><never/></notify>");
        let parsed = parse_dm_bookmark(ITEM_ID, &payload).expect("minimal dm-bookmark is valid");
        assert_eq!(parsed.jid.to_string(), ITEM_ID);
        let notify = parsed.notify.expect("notify present");
        assert!(is_notify_element(&notify));
        assert!(notify.has_child("never", "urn:xmpp:notification-settings:1"));
    }

    #[test]
    fn parse_no_notify_yields_none() {
        // A `<dm-bookmark>` with no `<notify>` is "no override".
        let payload = dm_bookmark_xml("");
        let parsed = parse_dm_bookmark(ITEM_ID, &payload).expect("empty dm-bookmark is valid");
        assert_eq!(parsed.jid.to_string(), ITEM_ID);
        assert!(parsed.notify.is_none());
    }

    #[test]
    fn build_then_parse_round_trips() {
        let notify: Element =
            "<notify xmlns='urn:xmpp:notification-settings:1'><on-mention/></notify>"
                .parse()
                .expect("valid notify");
        let element = build_dm_bookmark_element(&notify);
        assert_eq!(element.name(), "dm-bookmark");
        assert_eq!(element.ns(), NS_WADDLE_DM_BOOKMARKS_V0);

        let parsed = parse_dm_bookmark(ITEM_ID, &element).expect("round-trip parse");
        assert_eq!(parsed.jid.to_string(), ITEM_ID);
        assert_eq!(parsed.notify.as_ref(), Some(&notify));
    }

    #[test]
    fn parse_wrong_root_name_rejected() {
        let bad: Element = format!(
            "<conference xmlns='{NS_WADDLE_DM_BOOKMARKS_V0}'>\
                <notify xmlns='urn:xmpp:notification-settings:1'><never/></notify>\
             </conference>"
        )
        .parse()
        .expect("valid xml");
        assert!(matches!(
            parse_dm_bookmark(ITEM_ID, &bad),
            Err(DmBookmarkError::WrongRoot { .. })
        ));
    }

    #[test]
    fn parse_wrong_namespace_rejected() {
        let bad: Element = "<dm-bookmark xmlns='urn:example:other'>\
                <notify xmlns='urn:xmpp:notification-settings:1'><never/></notify>\
             </dm-bookmark>"
            .parse()
            .expect("valid xml");
        assert!(matches!(
            parse_dm_bookmark(ITEM_ID, &bad),
            Err(DmBookmarkError::WrongRoot { .. })
        ));
    }

    #[test]
    fn parse_domain_only_item_id_rejected() {
        let payload =
            dm_bookmark_xml("<notify xmlns='urn:xmpp:notification-settings:1'><never/></notify>");
        assert_eq!(
            parse_dm_bookmark("example.com", &payload).unwrap_err(),
            DmBookmarkError::InvalidJid("example.com".to_string())
        );
    }

    #[test]
    fn parse_empty_item_id_rejected() {
        let payload =
            dm_bookmark_xml("<notify xmlns='urn:xmpp:notification-settings:1'><never/></notify>");
        assert_eq!(
            parse_dm_bookmark("", &payload).unwrap_err(),
            DmBookmarkError::InvalidJid(String::new())
        );
    }

    #[test]
    fn parse_unknown_child_rejected() {
        let payload = dm_bookmark_xml("<weird xmlns='urn:example:other'/>");
        assert!(matches!(
            parse_dm_bookmark(ITEM_ID, &payload),
            Err(DmBookmarkError::UnknownChild(_))
        ));
    }

    #[test]
    fn parse_multiple_notify_rejected() {
        let payload = dm_bookmark_xml(
            "<notify xmlns='urn:xmpp:notification-settings:1'><never/></notify>\
             <notify xmlns='urn:xmpp:notification-settings:1'><always/></notify>",
        );
        assert_eq!(
            parse_dm_bookmark(ITEM_ID, &payload).unwrap_err(),
            DmBookmarkError::MultipleNotify
        );
    }

    #[test]
    fn parse_malformed_notify_propagates_as_invalid_notify() {
        // Two account-wide fallback settings violate XEP-0492 §3 and the
        // shared validator returns MultipleFallbackSettings, which must
        // surface as InvalidNotify (via #[from]).
        let payload = dm_bookmark_xml(
            "<notify xmlns='urn:xmpp:notification-settings:1'><always/><never/></notify>",
        );
        assert_eq!(
            parse_dm_bookmark(ITEM_ID, &payload).unwrap_err(),
            DmBookmarkError::InvalidNotify(NotificationSettingsError::MultipleFallbackSettings)
        );
    }

    #[test]
    fn parse_rich_payload_round_trips_and_preserves_advanced() {
        // The XEP-0492 §2.3 <advanced/> opt-in (#719) rides inside the
        // hosted <notify>; the cloned notify must preserve it verbatim.
        let payload = dm_bookmark_xml(
            "<notify xmlns='urn:xmpp:notification-settings:1'>\
                <on-mention>\
                    <advanced>\
                        <rich-payload xmlns='urn:waddle:push:rich:0'/>\
                    </advanced>\
                </on-mention>\
             </notify>",
        );
        let parsed = parse_dm_bookmark(ITEM_ID, &payload).expect("rich payload is valid");
        let notify = parsed.notify.expect("notify present");
        let advanced = notify
            .get_child("on-mention", "urn:xmpp:notification-settings:1")
            .expect("on-mention setting")
            .get_child("advanced", "urn:xmpp:notification-settings:1")
            .expect("advanced preserved");
        assert!(advanced.has_child("rich-payload", "urn:waddle:push:rich:0"));

        // Round-trip through the builder keeps the advanced child.
        let rebuilt = build_dm_bookmark_element(&notify);
        let reparsed = parse_dm_bookmark(ITEM_ID, &rebuilt).expect("round-trip parse");
        assert_eq!(reparsed.notify.as_ref(), Some(&notify));
    }

    #[test]
    fn parse_attribute_on_dm_bookmark_rejected() {
        let bad: Element = format!(
            "<dm-bookmark xmlns='{NS_WADDLE_DM_BOOKMARKS_V0}' foo='bar'>\
                <notify xmlns='urn:xmpp:notification-settings:1'><never/></notify>\
             </dm-bookmark>"
        )
        .parse()
        .expect("valid xml");
        assert_eq!(
            parse_dm_bookmark(ITEM_ID, &bad).unwrap_err(),
            DmBookmarkError::UnexpectedAttribute("foo".to_string())
        );
    }

    #[test]
    fn parse_namespaced_attribute_on_dm_bookmark_rejected() {
        let bad: Element = "<dm-bookmark xmlns='urn:waddle:dm-bookmarks:0' \
                xmlns:other='urn:example:other' other:foo='bar'>\
                <notify xmlns='urn:xmpp:notification-settings:1'><never/></notify>\
             </dm-bookmark>"
            .parse()
            .expect("valid xml");
        assert!(matches!(
            parse_dm_bookmark(ITEM_ID, &bad),
            Err(DmBookmarkError::NamespacedAttribute(_))
        ));
    }

    #[test]
    fn parse_text_content_rejected() {
        let bad = dm_bookmark_xml(
            "oops<notify xmlns='urn:xmpp:notification-settings:1'><never/></notify>",
        );
        assert_eq!(
            parse_dm_bookmark(ITEM_ID, &bad).unwrap_err(),
            DmBookmarkError::UnexpectedTextContent
        );
    }

    #[test]
    fn parse_whitespace_only_text_accepted() {
        let element: Element = format!(
            "<dm-bookmark xmlns='{NS_WADDLE_DM_BOOKMARKS_V0}'>\n  \
                <notify xmlns='urn:xmpp:notification-settings:1'><never/></notify>\n\
             </dm-bookmark>"
        )
        .parse()
        .expect("valid xml");
        parse_dm_bookmark(ITEM_ID, &element).expect("whitespace-only text must round-trip");
    }

    #[test]
    fn is_dm_bookmarks_node_matches_only_the_node() {
        assert!(is_dm_bookmarks_node(PEP_NODE_WADDLE_DM_BOOKMARKS));
        assert!(!is_dm_bookmarks_node("urn:xmpp:bookmarks:1"));
        assert!(!is_dm_bookmarks_node(""));
    }

    /// Pin the `waddle-xmpp`-side namespace constant to its sibling in
    /// `waddle-xmpp-core` so a rename in either crate fails CI.
    #[test]
    fn pep_node_waddle_dm_bookmarks_constant_matches_core() {
        assert_eq!(
            NS_WADDLE_DM_BOOKMARKS_V0,
            waddle_xmpp_core::pubsub::PEP_NODE_WADDLE_DM_BOOKMARKS
        );
        assert_eq!(
            PEP_NODE_WADDLE_DM_BOOKMARKS,
            waddle_xmpp_core::pubsub::PEP_NODE_WADDLE_DM_BOOKMARKS
        );
    }
}

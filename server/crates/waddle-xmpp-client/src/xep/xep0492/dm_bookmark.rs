//! Waddle DM-bookmark carrier for XEP-0492 notification settings.

use jid::BareJid;
use minidom::Element;

use crate::pep::NS_PUBSUB;

use super::{read_rich_payload_opt_in, NotifyMode, NS_NOTIFICATION_SETTINGS};

/// `jabber:client` stream namespace for the IQ envelopes the DM-carrier
/// builders emit.
const NS_CLIENT: &str = "jabber:client";

/// Waddle-custom DM-bookmark carrier namespace + PEP node (issue #720).
///
/// The DM counterpart to the XEP-0402 MUC bookmarks node: a
/// Waddle-custom PEP carrier that hosts a single official XEP-0492
/// `<notify>` per direct-chat contact (PEP item id == the contact's
/// bare JID). XEP-0402 is conference-only and no XEP-defined "DM
/// bookmark" exists, so the carrier lives in the project-local
/// `urn:waddle:dm-bookmarks:0` namespace per the CLAUDE.md XEP-
/// conformance hard rule. See `docs/specs/urn-waddle-dm-bookmarks.md`.
///
/// The client keeps its own copy (it does NOT depend on the server
/// `waddle-xmpp` crate). It MUST stay byte-compatible with the server
/// module
/// `waddle_xmpp::xep::xep_waddle_dm_bookmarks::PEP_NODE_WADDLE_DM_BOOKMARKS`
/// and the core constant
/// `waddle_xmpp_core::pubsub::pep::PEP_NODE_WADDLE_DM_BOOKMARKS`, whose
/// value is `urn:waddle:dm-bookmarks:0` (node name == namespace, as in
/// XEP-0402).
pub const NS_WADDLE_DM_BOOKMARKS: &str = "urn:waddle:dm-bookmarks:0";

/// Finite `pubsub#max_items` cap requested for the DM-bookmarks node.
///
/// MUST stay byte-compatible with the server node default
/// `waddle_xmpp_core::pubsub::PEP_BOOKMARK_MAX_ITEMS` (the client crate
/// keeps its own copy — it does not depend on the server crates). The
/// DM node deliberately caps retention rather than requesting `max`
/// (`u32::MAX`): an unbounded node disables server-side eviction, so a
/// client could grow its own DM node without limit. The publish-options
/// request below therefore pins the same finite cap the node defaults
/// to, so the requested config matches what the server creates on first
/// publish (anti-DoS parity with the XEP-0402 bookmarks node).
pub const DM_BOOKMARK_MAX_ITEMS: u32 = 1024;

/// DM-carrier retract predicate (issue #720, sparse / override-only).
///
/// Returns `true` iff a merged `<notify/>` carries NOTHING beyond the
/// XEP-0492 §3 conversation default `default_mode` (for direct chats,
/// `always`). An item exists on the DM node ONLY when the DM has an
/// override; when this returns `true` the caller retracts the item
/// (absence == the §3 default). Concretely, the `<notify/>` is
/// default-only when ALL of:
///
/// * its fallback mode equals `default_mode` (the no-attrs setting),
/// * the Waddle rich-payload opt-in (#719) is NOT set on any setting,
/// * there are no identity-scoped sibling settings (carrying
///   `identity-category` / `identity-type`) written by another client,
/// * no setting carries a foreign `<advanced/>` child.
///
/// A `<notify/>` with no fallback child at all (only identity-scoped
/// siblings) is therefore NOT default — the siblings are foreign state
/// we must keep on the wire.
pub fn dm_notify_is_default(notify: &Element, default_mode: NotifyMode) -> bool {
    // Rich opt-in is honored on ANY setting (matching the readers), so
    // an opt-in anywhere means the DM carries an override.
    if read_rich_payload_opt_in(notify) {
        return false;
    }
    // Any direct child of `<notify/>` that is NOT a recognized XEP-0492
    // setting element (a foreign namespace, or an unknown name in the
    // notification-settings namespace) is foreign/unknown state. Be
    // conservative: treat the `<notify/>` as a non-default override so
    // the item is preserved rather than retracted, which would drop the
    // foreign state `merge_notify` otherwise carries verbatim (Copilot
    // review). The server's `validate_notify_element` already rejects
    // such children, so this is a defence-in-depth guard for inputs that
    // did not pass through that gate.
    let all_children_are_settings = notify.children().all(|child| {
        child.ns() == NS_NOTIFICATION_SETTINGS && NotifyMode::from_wire_name(child.name()).is_some()
    });
    if !all_children_are_settings {
        return false;
    }
    let settings: Vec<&Element> = notify
        .children()
        .filter(|child| {
            child.ns() == NS_NOTIFICATION_SETTINGS
                && NotifyMode::from_wire_name(child.name()).is_some()
        })
        .collect();
    let mut saw_default_fallback = false;
    for setting in settings {
        let identity_scoped =
            setting.attr("identity-category").is_some() || setting.attr("identity-type").is_some();
        if identity_scoped {
            // Foreign identity-scoped state — never the bare default.
            return false;
        }
        // Any `<advanced/>` on the no-attrs fallback is foreign (our own
        // `<rich-payload/>` was already ruled out above), so it's an
        // override worth keeping.
        if setting.has_child("advanced", NS_NOTIFICATION_SETTINGS) {
            return false;
        }
        match NotifyMode::from_wire_name(setting.name()) {
            Some(found) if found == default_mode => saw_default_fallback = true,
            // A non-default fallback (or any unrecognized mode) is an
            // override.
            _ => return false,
        }
    }
    saw_default_fallback
}

/// Build the `<dm-bookmark xmlns='urn:waddle:dm-bookmarks:0'>` PEP
/// payload directly hosting one XEP-0492 `<notify/>` (issue #720).
///
/// No `<extensions/>` wrapper and no native field — a DM has no
/// autojoin / nick / password. The `<notify/>` is cloned in verbatim;
/// its namespace and children stay byte-identical to official XEP-0492
/// (Waddle hosts it, it does not fork it). Pure.
pub fn build_dm_bookmark_element(notify: &Element) -> Element {
    Element::builder("dm-bookmark", NS_WADDLE_DM_BOOKMARKS)
        .append(notify.clone())
        .build()
}

/// Read the XEP-0492 `<notify/>` child of a `<dm-bookmark>` payload
/// (issue #720), if present. Returns `None` for a malformed payload
/// missing the `<notify/>`. Borrows.
pub fn read_dm_bookmark_notify(payload: &Element) -> Option<&Element> {
    payload.get_child("notify", NS_NOTIFICATION_SETTINGS)
}

/// Build a `get` IQ requesting the user's DM-bookmark items from their
/// own PEP [`NS_WADDLE_DM_BOOKMARKS`] node (issue #720).
///
/// `to=` is omitted so the server routes the request to the account's
/// own PEP service (XEP-0163 §3.5), mirroring
/// [`crate::xep::xep0402::build_fetch_bookmarks_iq`].
pub fn build_fetch_dm_bookmarks_iq(request_id: &str) -> Element {
    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), request_id)
        .append(
            Element::builder("pubsub", NS_PUBSUB)
                .append(
                    Element::builder("items", NS_PUBSUB)
                        .attr(
                            minidom::rxml::xml_ncname!("node").to_owned(),
                            NS_WADDLE_DM_BOOKMARKS,
                        )
                        .build(),
                )
                .build(),
        )
        .build()
}

/// Build a `set` IQ that publishes one DM-bookmark item to the user's
/// own PEP [`NS_WADDLE_DM_BOOKMARKS`] node (issue #720).
///
/// The item id is the contact's bare JID; the payload is the
/// [`build_dm_bookmark_element`] wrapper directly hosting `notify`.
/// Pins the `publish-options` form so the server creates a private,
/// sparse node on first publish: `access_model=whitelist`,
/// `persist_items=true`, `send_last_published_item=never`, and a FINITE
/// `max_items` cap ([`DM_BOOKMARK_MAX_ITEMS`]). The cap matches the
/// server node default — unlike the XEP-0402 bookmark publish's
/// `max_items=max`, the DM node bounds retention so server-side eviction
/// stays enabled (anti-DoS parity with the bookmarks node). `to=` is
/// omitted (XEP-0163 §3.5).
pub fn build_publish_dm_bookmark_iq(jid: &BareJid, notify: &Element, request_id: &str) -> Element {
    let publish_options = Element::builder("publish-options", NS_PUBSUB)
        .append(
            Element::builder("x", "jabber:x:data")
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
                .append(
                    Element::builder("field", "jabber:x:data")
                        .attr(minidom::rxml::xml_ncname!("var").to_owned(), "FORM_TYPE")
                        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "hidden")
                        .append(
                            Element::builder("value", "jabber:x:data")
                                .append("http://jabber.org/protocol/pubsub#publish-options")
                                .build(),
                        )
                        .build(),
                )
                .append(submit_value_field("pubsub#persist_items", "true"))
                .append(submit_value_field(
                    "pubsub#max_items",
                    &DM_BOOKMARK_MAX_ITEMS.to_string(),
                ))
                .append(submit_value_field(
                    "pubsub#send_last_published_item",
                    "never",
                ))
                .append(submit_value_field("pubsub#access_model", "whitelist"))
                .build(),
        )
        .build();

    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), request_id)
        .append(
            Element::builder("pubsub", NS_PUBSUB)
                .append(
                    Element::builder("publish", NS_PUBSUB)
                        .attr(
                            minidom::rxml::xml_ncname!("node").to_owned(),
                            NS_WADDLE_DM_BOOKMARKS,
                        )
                        .append(
                            Element::builder("item", NS_PUBSUB)
                                .attr(minidom::rxml::xml_ncname!("id").to_owned(), jid.to_string())
                                .append(build_dm_bookmark_element(notify))
                                .build(),
                        )
                        .build(),
                )
                .append(publish_options)
                .build(),
        )
        .build()
}

/// Build a `set` IQ that retracts the DM-bookmark item id `jid` from
/// the user's own PEP [`NS_WADDLE_DM_BOOKMARKS`] node via XEP-0060
/// `<retract>` (issue #720).
///
/// The DM node is sparse / override-only: returning a DM to the
/// XEP-0492 §3 direct-chat default removes the item, so absence of an
/// item == the default. `notify='true'` asks the server to broadcast a
/// retract event to subscribers (mirroring XEP-0060 §7.2). `to=` is
/// omitted (XEP-0163 §3.5).
pub fn build_retract_dm_bookmark_iq(jid: &BareJid, request_id: &str) -> Element {
    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), request_id)
        .append(
            Element::builder("pubsub", NS_PUBSUB)
                .append(
                    Element::builder("retract", NS_PUBSUB)
                        .attr(
                            minidom::rxml::xml_ncname!("node").to_owned(),
                            NS_WADDLE_DM_BOOKMARKS,
                        )
                        .attr(minidom::rxml::xml_ncname!("notify").to_owned(), "true")
                        .append(
                            Element::builder("item", NS_PUBSUB)
                                .attr(minidom::rxml::xml_ncname!("id").to_owned(), jid.to_string())
                                .build(),
                        )
                        .build(),
                )
                .build(),
        )
        .build()
}

/// XEP-0004 `<field var='…'><value>…</value></field>` submit row for
/// the DM-carrier publish-options form.
fn submit_value_field(var: &str, value: &str) -> Element {
    Element::builder("field", "jabber:x:data")
        .attr(minidom::rxml::xml_ncname!("var").to_owned(), var)
        .append(
            Element::builder("value", "jabber:x:data")
                .append(value)
                .build(),
        )
        .build()
}

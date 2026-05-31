//! XEP-0492: Chat notification settings.
//!
//! Typed model + element round-trip for the
//! `urn:xmpp:notification-settings:1` `<notify/>` element.
//!
//! XEP-0492 v0.2.0 §2.1 places `<notify/>` inside the XEP-0402
//! bookmark `<extensions/>` of the conversation it applies to. This
//! module owns the wire ↔ typed translation for the **fallback**
//! element only (no `identity-category`/`identity-type` attrs) —
//! the v1 surface (#532) is account-wide.
//!
//! Foreign children inside `<advanced/>` and identity-scoped sibling
//! elements are preserved verbatim by [`merge_notify_into_extensions`]
//! so a Waddle client never destroys settings another client wrote
//! (XEP-0492 §3 first paragraph).

use jid::BareJid;
use minidom::Element;

use crate::pep::NS_PUBSUB;

/// `jabber:client` stream namespace for the IQ envelopes the DM-carrier
/// builders emit.
const NS_CLIENT: &str = "jabber:client";

/// XEP-0492 namespace.
pub const NS_NOTIFICATION_SETTINGS: &str = "urn:xmpp:notification-settings:1";

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

/// Waddle rich XEP-0357 push-summary opt-in namespace.
///
/// Mirrors `waddle_xmpp::xep::xep0492::NS_PUSH_RICH_PAYLOAD` (the
/// client crate keeps its own copy — it does not depend on the
/// server `waddle-xmpp` crate). XEP-0492 §2.3 reserves the optional
/// `<advanced/>` child for "finer-grained notification settings using
/// custom namespaces"; `<rich-payload xmlns='urn:waddle:push:rich:0'/>`
/// is the Waddle opt-in to a rich XEP-0357 summary carrying
/// `last-message-sender` + `last-message-body` (#719). The read/write
/// helpers here MUST stay byte-compatible with the server-side
/// `parse_rich_payload_opt_in`.
pub const NS_PUSH_RICH_PAYLOAD: &str = "urn:waddle:push:rich:0";

/// Read the Waddle rich-payload opt-in from a XEP-0492 `<notify/>`.
///
/// Returns `true` when any setting child (fallback or identity-scoped)
/// carries an `<advanced/>` holding a
/// `<rich-payload xmlns='urn:waddle:push:rich:0'/>` child. Mirrors the
/// server-side `parse_rich_payload_opt_in` so the write side here and
/// the read side there agree on one wire shape (#719). Absence of the
/// element is opt-out — the minimal XEP-0357 summary payload.
pub fn read_rich_payload_opt_in(notify: &Element) -> bool {
    notify
        .children()
        .filter(|child| child.ns() == NS_NOTIFICATION_SETTINGS)
        .filter_map(|setting| setting.get_child("advanced", NS_NOTIFICATION_SETTINGS))
        .any(|advanced| advanced.has_child("rich-payload", NS_PUSH_RICH_PAYLOAD))
}

/// Typed XEP-0492 fallback setting — the single child element under
/// `<notify/>` carrying no `identity-*` attributes.
///
/// XEP-0492 §2.1 defines three settings; this enum is exhaustive over
/// them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NotifyMode {
    /// `<always/>` — notify for every message.
    Always,
    /// `<on-mention/>` — only notify on a XEP-0461/0372 mention.
    OnMention,
    /// `<never/>` — never notify (muted).
    Never,
}

impl NotifyMode {
    /// Wire element name for this setting.
    pub fn as_wire_name(self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::OnMention => "on-mention",
            Self::Never => "never",
        }
    }

    /// Parse a wire element name back into a typed mode.
    pub fn from_wire_name(name: &str) -> Option<Self> {
        match name {
            "always" => Some(Self::Always),
            "on-mention" => Some(Self::OnMention),
            "never" => Some(Self::Never),
            _ => None,
        }
    }

    /// XSD child-sequence rank used to keep emitted `<notify/>`
    /// children in the order the XEP-0492 schema declares
    /// (`always` → `on-mention` → `never`). Round-8 XEP reviewer P2.
    fn xsd_rank(self) -> u8 {
        match self {
            Self::Always => 0,
            Self::OnMention => 1,
            Self::Never => 2,
        }
    }
}

/// Conversation-level default per XEP-0492 §3 last paragraph: in the
/// absence of any `<notify/>` element, `always` for direct chats and
/// private group chats, `on-mention` for public group chats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationKind {
    /// One-to-one chat (RFC 6121 §5).
    DirectChat,
    /// XEP-0045 group chat with `members-only` semantics.
    PrivateGroup,
    /// XEP-0045 group chat that is publicly discoverable.
    PublicGroup,
}

impl ConversationKind {
    /// Resolve the XEP-0492 §3 default when no fallback child is
    /// present.
    pub fn default_notify_mode(self) -> NotifyMode {
        match self {
            Self::DirectChat | Self::PrivateGroup => NotifyMode::Always,
            Self::PublicGroup => NotifyMode::OnMention,
        }
    }
}

/// Read the **fallback** notification setting from a `<notify/>`
/// element (no `identity-category`/`identity-type` attributes).
///
/// Returns `None` when no fallback child is present (a `<notify/>`
/// carrying only identity-scoped siblings written by another client).
/// Caller resolves `None` against [`ConversationKind::default_notify_mode`].
pub fn read_fallback_mode(notify: &Element) -> Option<NotifyMode> {
    notify.children().find_map(|child| {
        if child.ns() != NS_NOTIFICATION_SETTINGS
            || child.attr("identity-category").is_some()
            || child.attr("identity-type").is_some()
        {
            return None;
        }
        NotifyMode::from_wire_name(child.name())
    })
}

/// Merge a desired [`NotifyMode`] into an existing XEP-0402
/// `<extensions/>` element, returning a fresh `<extensions/>` with
/// the fallback `<notify/>` child replaced and all other children
/// preserved.
///
/// Semantics:
///
/// * Foreign children inside `<extensions/>` (non-`<notify/>`) are
///   carried over verbatim — other extensions own them.
/// * Multiple `<notify/>` siblings (a malformed but possible state)
///   are folded into a single `<notify/>` on output (round-7 XEP
///   reviewer pinning of §3 third paragraph). Duplicate setting
///   elements with identical name+attrs are de-duplicated.
/// * Identity-scoped setting siblings (carrying `identity-category`
///   or `identity-type`) are preserved verbatim — they belong to
///   another client's identity and we do not own them.
/// * The single existing fallback child (no `identity-*` attrs) is
///   replaced with the requested mode. XEP-0492 §3 third paragraph
///   forbids more than one element with the same name + attrs.
/// * When the user changes the fallback **name** (e.g. always →
///   never), the `<advanced/>` child attached to the prior fallback
///   is **moved verbatim under the new fallback parent**. XEP-0492
///   §3 ¶1 *"MUST NOT delete or alter the `<advanced/>` notification
///   settings they do not support when updating the notification
///   settings for a given conversation"* — moving the unchanged
///   element to a new parent preserves the foreign rules while
///   honoring the user's explicit fallback override. Dropping the
///   block would violate the MUST NOT; promoting it to an
///   identity-scoped sibling would require synthesizing a disco
///   identity Waddle hasn't registered. The trade-off (the original
///   writing client picked the parent name as the closest non-
///   advanced fallback per §3 ¶5; our move re-parents under a
///   different fallback name) is the least-bad option round-10 of
///   the XMPP-compliance review settled on.
/// * When the merge is a no-op (the existing fallback already has
///   the requested mode), the `<advanced/>` child is preserved
///   verbatim — nothing to invalidate.
///
/// * The Waddle rich XEP-0357 push-summary opt-in
///   (`<advanced><rich-payload xmlns='urn:waddle:push:rich:0'/></advanced>`,
///   #719) is **ours** to manage, so `rich_payload_opt_in` toggles it
///   on the new fallback: `true` ensures exactly one `<rich-payload/>`
///   inside the fallback's `<advanced/>`; `false` removes it. Foreign
///   `<advanced/>` children are preserved either way (§3 ¶1 MUST NOT
///   delete/alter unsupported advanced settings); if removing our
///   `<rich-payload/>` leaves the `<advanced/>` empty it is dropped
///   (§2.3 `<advanced/>` SHOULD NOT be empty).
///
/// The function is pure — it takes a borrowed `extensions` element
/// and returns an owned new element.
pub fn merge_notify_into_extensions(
    extensions: Option<&Element>,
    mode: NotifyMode,
    rich_payload_opt_in: bool,
) -> Element {
    let mut builder = Element::builder("extensions", crate::pep::NS_BOOKMARKS);
    let mut notify_setting_children: Vec<Element> = Vec::new();

    if let Some(existing) = extensions {
        for child in existing.children() {
            if child.is("notify", NS_NOTIFICATION_SETTINGS) {
                gather_notify_settings(child, &mut notify_setting_children);
            } else if child.ns() == crate::pep::NS_BOOKMARKS {
                // XEP-0402 XSD `<extensions/>` content is
                // `<xs:any namespace='##other'>` — drop any
                // malformed same-namespace child so the republish
                // is schema-valid (round-12 Copilot review).
            } else {
                builder = builder.append(child.clone());
            }
        }
    }

    builder
        .append(build_merged_notify_element(
            notify_setting_children,
            mode,
            rich_payload_opt_in,
        ))
        .build()
}

/// Gather the setting children of one `<notify/>` element into `out`,
/// de-duplicating identical `(name, ns, identity-*)` elements per
/// XEP-0492 v0.2.0 §3 ¶3. Folds multiple `<notify/>` siblings into one
/// pool when called repeatedly with the same `out` vector (the
/// malformed-but-possible multi-`<notify/>` case the merge collapses).
fn gather_notify_settings(notify: &Element, out: &mut Vec<Element>) {
    for setting in notify.children() {
        if out.iter().any(|prior| settings_equivalent(prior, setting)) {
            continue;
        }
        out.push(setting.clone());
    }
}

/// Parent-agnostic XEP-0492 `<notify/>` merge core.
///
/// Both carriers reuse this: the MUC carrier (XEP-0402
/// `<extensions/>` → [`merge_notify_into_extensions`]) and the Waddle
/// DM carrier (`<dm-bookmark>`, issue #720) host the SAME `<notify/>`
/// shape but at different nesting depths — MUC wraps it in
/// `<extensions/>`, the DM hosts it directly. This function takes the
/// existing `<notify/>` (if any) and produces the merged `<notify/>`,
/// so the §3-preservation logic lives in exactly one place.
///
/// Semantics are identical to [`merge_notify_into_extensions`]: the
/// single no-attrs fallback is replaced with `mode`; identity-scoped
/// siblings and foreign `<advanced/>` children are preserved verbatim
/// (XEP-0492 §3 ¶1); multiple `<notify/>` settings are de-duplicated
/// and folded; the Waddle `<rich-payload/>` opt-in (#719) is toggled
/// by `rich_payload_opt_in`. Pure — borrows the input, returns an
/// owned `<notify/>`.
pub fn merge_notify(
    existing_notify: Option<&Element>,
    mode: NotifyMode,
    rich_payload_opt_in: bool,
) -> Element {
    let mut notify_setting_children: Vec<Element> = Vec::new();
    if let Some(existing) = existing_notify {
        gather_notify_settings(existing, &mut notify_setting_children);
    }
    build_merged_notify_element(notify_setting_children, mode, rich_payload_opt_in)
}

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
/// Pins the same `publish-options` form the XEP-0402 bookmark publish
/// uses (`access_model=whitelist`, `persist_items=true`,
/// `max_items=max`, `send_last_published_item=never`) so the server
/// creates a private, sparse node on first publish. `to=` is omitted
/// (XEP-0163 §3.5).
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
                .append(submit_value_field("pubsub#max_items", "max"))
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

/// Build the single `<notify/>` output element from the deduplicated
/// list of setting children gathered from the input, applying the
/// fallback-replace rule for the user's chosen `mode`.
///
/// Spec-conformance choices (XEP-0492 v0.2.0 §3, round-10 XMPP
/// review):
///
/// * §3 ¶1 *"Applications implementing this specification MUST NOT
///   delete or alter the `<advanced/>` notification settings they
///   do not support when updating the notification settings for a
///   given conversation"* — when the user changes the fallback
///   mode name and the previous fallback carried an `<advanced/>`
///   block, **the `<advanced/>` element is moved verbatim under
///   the new fallback parent**. The `<advanced/>` content itself is
///   unchanged; only its enclosing setting-element name changes.
///   The reverse choice (drop the `<advanced/>`) is the simpler
///   read but violates the MUST NOT delete. The third option
///   (preserve the old fallback as an identity-scoped sibling) was
///   rejected because Waddle has no registered Service Discovery
///   identity, so synthesizing an identity-category attr would
///   leak the preserved rules to any reading client that shares
///   the synthesized identity. Moving the foreign element verbatim
///   is the least-bad option.
///
/// * §3 ¶3 *"There SHALL NOT be more than one notification element
///   having the same name, attributes and attribute values"* — on
///   the no-attrs fallback slot we emit exactly one element (for
///   the user's chosen `mode`). Any extra no-attrs setting children
///   in the input (a malformed but possible state) collapse into
///   that single output; their `<advanced/>` children, if present,
///   are merged onto the new fallback so no rules are lost.
///
/// Emitted children are sorted into the XEP-0492 XSD sequence
/// (`always` → `on-mention` → `never`), with identity-scoped
/// siblings ordered after the no-attrs fallback within each mode
/// group — round-8 XEP reviewer P2.
fn build_merged_notify_element(
    setting_children: Vec<Element>,
    mode: NotifyMode,
    rich_payload_opt_in: bool,
) -> Element {
    let mut identity_scoped: Vec<Element> = Vec::new();
    let mut preserved_advanced: Vec<Element> = Vec::new();
    let mut foreign: Vec<Element> = Vec::new();

    for child in setting_children {
        let is_setting = child.ns() == NS_NOTIFICATION_SETTINGS
            && NotifyMode::from_wire_name(child.name()).is_some();
        let has_identity_attr =
            child.attr("identity-category").is_some() || child.attr("identity-type").is_some();
        if is_setting && has_identity_attr {
            // Identity-scoped sibling. Foreign rules are preserved
            // verbatim (§3 ¶1), but our OWN `<rich-payload/>` marker is
            // ours to manage: on opt-out we strip it here too, so the
            // opt-in readers (client `read_rich_payload_opt_in` and the
            // server `parse_rich_payload_opt_in`, both of which honor
            // the marker on ANY setting) can't keep reporting opted-in
            // after the user opted out. On opt-in we leave the sibling
            // untouched — the user's opt-in is recorded on the fallback.
            identity_scoped.push(if rich_payload_opt_in {
                child
            } else {
                strip_rich_payload_from_setting(child)
            });
        } else if is_setting {
            // No-attrs fallback (possibly more than one, in
            // malformed input). Salvage any `<advanced/>` child
            // for the new fallback (§3 ¶1 MUST NOT delete) — the
            // setting parent itself is collapsed into the single
            // output fallback below.
            for inner in child.children() {
                if inner.is("advanced", NS_NOTIFICATION_SETTINGS) {
                    preserved_advanced.push(inner.clone());
                }
            }
        } else {
            // Foreign child or other non-spec element — pass through.
            foreign.push(child);
        }
    }

    // Build the single output fallback for the user's chosen mode,
    // re-parenting any preserved `<advanced/>` children under it and
    // toggling the Waddle `<rich-payload/>` opt-in (#719).
    let mut new_fallback = Element::builder(mode.as_wire_name(), NS_NOTIFICATION_SETTINGS);
    for advanced in apply_rich_payload(preserved_advanced, rich_payload_opt_in) {
        new_fallback = new_fallback.append(advanced);
    }
    let new_fallback = new_fallback.build();

    let mut emit: Vec<Element> = Vec::with_capacity(1 + identity_scoped.len() + foreign.len());
    emit.push(new_fallback);
    emit.extend(identity_scoped);
    emit.extend(foreign);

    // Sort by (XSD mode rank, identity-category, identity-type).
    // Unknown element names (foreign children) sort after the spec
    // settings so the canonical XSD prefix appears first.
    // `Vec::sort_by` is stable per std (https://doc.rust-lang.org/
    // std/vec/struct.Vec.html#method.sort_by) so the relative order
    // of equal-key elements is preserved for debuggability and
    // interop — the unstable variant would be `sort_unstable_by`.
    emit.sort_by(|a, b| sort_key(a).cmp(&sort_key(b)));

    let mut builder = Element::builder("notify", NS_NOTIFICATION_SETTINGS);
    for child in emit {
        builder = builder.append(child);
    }
    builder.build()
}

/// Normalize the `<advanced/>` blocks salvaged from the prior fallback
/// onto the new fallback, toggling the Waddle `<rich-payload/>` opt-in.
///
/// XEP-0492 forbids more than one `<advanced/>` per setting (the server
/// validator rejects it), so the salvaged blocks are folded into a
/// single `<advanced/>` whose foreign children are preserved verbatim
/// and in order (§3 ¶1 MUST NOT delete or alter unsupported advanced
/// settings). Our own `<rich-payload xmlns='urn:waddle:push:rich:0'/>`
/// is stripped first (we own it) and re-added exactly once iff
/// `opt_in`, so the function is idempotent over repeated merges. When
/// the result would be empty the `<advanced/>` is omitted entirely
/// (§2.3 `<advanced/>` SHOULD NOT be empty). Returns at most one
/// element.
fn apply_rich_payload(advanced_blocks: Vec<Element>, opt_in: bool) -> Vec<Element> {
    let mut children: Vec<Element> = Vec::new();
    for advanced in advanced_blocks {
        for child in advanced.children() {
            // Our own opt-in marker — drop here, re-add below so a
            // repeated merge never accumulates duplicates.
            if child.is("rich-payload", NS_PUSH_RICH_PAYLOAD) {
                continue;
            }
            children.push(child.clone());
        }
    }
    if opt_in {
        children.push(Element::builder("rich-payload", NS_PUSH_RICH_PAYLOAD).build());
    }
    if children.is_empty() {
        return Vec::new();
    }
    let mut advanced = Element::builder("advanced", NS_NOTIFICATION_SETTINGS);
    for child in children {
        advanced = advanced.append(child);
    }
    vec![advanced.build()]
}

/// Return a copy of an identity-scoped setting element with our
/// `<rich-payload xmlns='urn:waddle:push:rich:0'/>` marker removed from
/// its `<advanced/>` child, dropping a thereby-emptied `<advanced/>`
/// (§2.3 SHOULD NOT be empty). All foreign children — including foreign
/// `<advanced/>` rules and the setting's `identity-*` attributes — are
/// preserved verbatim (§3 ¶1). Used on opt-out so the marker is cleared
/// wherever a (cross-client) writer may have placed it, keeping the
/// write side symmetric with the read side. #719.
fn strip_rich_payload_from_setting(setting: Element) -> Element {
    let mut builder = Element::builder(setting.name(), setting.ns());
    for ((ns, name), value) in setting.attrs().iter() {
        builder = builder.attr_ns(ns.clone(), name.clone(), value);
    }
    for child in setting.children() {
        if child.is("advanced", NS_NOTIFICATION_SETTINGS) {
            let kept: Vec<Element> = child
                .children()
                .filter(|grandchild| !grandchild.is("rich-payload", NS_PUSH_RICH_PAYLOAD))
                .cloned()
                .collect();
            if kept.is_empty() {
                continue;
            }
            let mut advanced = Element::builder("advanced", NS_NOTIFICATION_SETTINGS);
            for grandchild in kept {
                advanced = advanced.append(grandchild);
            }
            builder = builder.append(advanced.build());
        } else {
            builder = builder.append(child.clone());
        }
    }
    builder.build()
}

fn sort_key(el: &Element) -> (u8, Option<&str>, Option<&str>) {
    // Only spec setting elements get a real rank — foreign children
    // (including any that happen to share a local name with `always`
    // / `on-mention` / `never` but live in a different namespace)
    // sort last with rank u8::MAX. Round-9 reviewer P2 — without the
    // namespace check, a `<always xmlns='custom'/>` inside a
    // `<notify/>` would interleave with the spec block.
    let mode_rank = if el.ns() == NS_NOTIFICATION_SETTINGS {
        NotifyMode::from_wire_name(el.name())
            .map(|m| m.xsd_rank())
            .unwrap_or(u8::MAX)
    } else {
        u8::MAX
    };
    (
        mode_rank,
        el.attr("identity-category"),
        el.attr("identity-type"),
    )
}

/// Two setting elements are equivalent (for §3 ¶3 dedupe) when they
/// share the same name, namespace, and the identity attribute pair.
/// XEP-0492 v0.2.0 §3 ¶3 forbids "more than one notification element
/// having the same name, attributes and attribute values"; the XSD
/// declares only `identity-category` / `identity-type` attrs on
/// setting elements, so for any spec-valid input this is the
/// authoritative key. The compare is by full namespaced + local name
/// to defend against malformed input that mixes spec elements with
/// foreign ones at the same level.
fn settings_equivalent(a: &Element, b: &Element) -> bool {
    a.name() == b.name()
        && a.ns() == b.ns()
        && a.attr("identity-category") == b.attr("identity-category")
        && a.attr("identity-type") == b.attr("identity-type")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_round_trips_wire_name() {
        for mode in [NotifyMode::Always, NotifyMode::OnMention, NotifyMode::Never] {
            assert_eq!(NotifyMode::from_wire_name(mode.as_wire_name()), Some(mode));
        }
    }

    #[test]
    fn from_wire_name_rejects_unknown() {
        assert!(NotifyMode::from_wire_name("sometimes").is_none());
        assert!(NotifyMode::from_wire_name("").is_none());
    }

    #[test]
    fn default_mode_matches_xep0492_section_3() {
        assert_eq!(
            ConversationKind::DirectChat.default_notify_mode(),
            NotifyMode::Always
        );
        assert_eq!(
            ConversationKind::PrivateGroup.default_notify_mode(),
            NotifyMode::Always
        );
        assert_eq!(
            ConversationKind::PublicGroup.default_notify_mode(),
            NotifyMode::OnMention
        );
    }

    fn find_notify_in_extensions(extensions: &Element) -> Option<&Element> {
        extensions
            .children()
            .find(|child| child.is("notify", NS_NOTIFICATION_SETTINGS))
    }

    #[test]
    fn merge_into_empty_extensions_emits_single_fallback_child() {
        let extensions = merge_notify_into_extensions(None, NotifyMode::OnMention, false);
        let elem = find_notify_in_extensions(&extensions).expect("notify present");
        assert_eq!(elem.name(), "notify");
        assert_eq!(elem.ns(), NS_NOTIFICATION_SETTINGS);
        let children: Vec<_> = elem.children().collect();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].name(), "on-mention");
        assert_eq!(children[0].ns(), NS_NOTIFICATION_SETTINGS);
        assert!(children[0].attr("identity-category").is_none());
    }

    #[test]
    fn read_fallback_skips_identity_scoped_siblings() {
        let xml = "<notify xmlns='urn:xmpp:notification-settings:1'>\
                    <never identity-category='client' identity-type='pc' />\
                    <on-mention identity-category='client' identity-type='phone' />\
                    <always />\
                    </notify>";
        let elem: Element = xml.parse().expect("valid xml");
        assert_eq!(read_fallback_mode(&elem), Some(NotifyMode::Always));
    }

    #[test]
    fn read_fallback_returns_none_when_only_identity_scoped() {
        let xml = "<notify xmlns='urn:xmpp:notification-settings:1'>\
                    <never identity-category='client' />\
                    </notify>";
        let elem: Element = xml.parse().expect("valid xml");
        assert!(read_fallback_mode(&elem).is_none());
    }

    #[test]
    fn merge_inserts_notify_into_empty_extensions() {
        let merged = merge_notify_into_extensions(None, NotifyMode::Never, false);
        assert_eq!(merged.name(), "extensions");
        let notify = find_notify_in_extensions(&merged).expect("notify present");
        assert_eq!(read_fallback_mode(notify), Some(NotifyMode::Never));
    }

    #[test]
    fn merge_replaces_existing_fallback_only() {
        let extensions_xml = "<extensions xmlns='urn:xmpp:bookmarks:1'>\
                    <notify xmlns='urn:xmpp:notification-settings:1'>\
                        <never identity-category='client' identity-type='pc' />\
                        <always />\
                    </notify>\
                    </extensions>";
        let extensions: Element = extensions_xml.parse().expect("valid xml");
        let merged = merge_notify_into_extensions(Some(&extensions), NotifyMode::OnMention, false);

        let notify = find_notify_in_extensions(&merged).expect("notify present");
        // Identity-scoped sibling preserved verbatim.
        assert!(notify.children().any(|child| {
            child.name() == "never"
                && child.attr("identity-category") == Some("client")
                && child.attr("identity-type") == Some("pc")
        }));
        // Fallback rewritten to on-mention.
        assert_eq!(read_fallback_mode(notify), Some(NotifyMode::OnMention));
        // §3: only one fallback element.
        let fallback_count = notify
            .children()
            .filter(|child| {
                child.ns() == NS_NOTIFICATION_SETTINGS
                    && NotifyMode::from_wire_name(child.name()).is_some()
                    && child.attr("identity-category").is_none()
                    && child.attr("identity-type").is_none()
            })
            .count();
        assert_eq!(fallback_count, 1);
    }

    #[test]
    fn merge_moves_advanced_under_new_fallback_when_name_changes() {
        // Round-10 XMPP-conformance reviewer P1 — XEP-0492 §3 ¶1
        // "MUST NOT delete or alter the `<advanced />` notification
        // settings they do not support when updating the notification
        // settings for a given conversation". The `<advanced/>`
        // element MUST survive the fallback-name change; we move it
        // verbatim to the new fallback parent.
        let extensions_xml = "<extensions xmlns='urn:xmpp:bookmarks:1'>\
                    <notify xmlns='urn:xmpp:notification-settings:1'>\
                        <always>\
                            <advanced xmlns='urn:xmpp:notification-settings:1'>\
                                <weekend xmlns='custom:other-client:1'/>\
                            </advanced>\
                        </always>\
                    </notify>\
                    </extensions>";
        let extensions: Element = extensions_xml.parse().expect("valid xml");
        let merged = merge_notify_into_extensions(Some(&extensions), NotifyMode::Never, false);
        let notify = find_notify_in_extensions(&merged).expect("notify present");

        assert_eq!(read_fallback_mode(notify), Some(NotifyMode::Never));
        let never_elem = notify
            .children()
            .find(|child| {
                child.name() == "never"
                    && child.attr("identity-category").is_none()
                    && child.attr("identity-type").is_none()
            })
            .expect("rewritten never present");
        let advanced = never_elem
            .get_child("advanced", NS_NOTIFICATION_SETTINGS)
            .expect("advanced moved under new fallback");
        assert!(advanced
            .children()
            .any(|c| c.name() == "weekend" && c.ns() == "custom:other-client:1"));
    }

    #[test]
    fn merge_never_emits_same_namespace_extensions_children() {
        // Round-10 XMPP-conformance reviewer P2 — XEP-0402 XSD
        // declares `<extensions/>` content as `<xs:any namespace='##other'>`,
        // i.e. children MUST be in a namespace other than
        // `urn:xmpp:bookmarks:1`. Our writer must never produce a
        // child in the bookmarks namespace.
        let merged = merge_notify_into_extensions(None, NotifyMode::Always, false);
        for child in merged.children() {
            assert_ne!(
                child.ns(),
                crate::pep::NS_BOOKMARKS,
                "extensions child MUST be in a non-bookmarks namespace per XEP-0402 XSD"
            );
        }
    }

    #[test]
    fn merge_collapses_multiple_no_attrs_fallbacks() {
        // Round-10 §3 ¶3 corner case: malformed input with two
        // no-attrs settings (different names) MUST yield exactly
        // one no-attrs fallback on output (the user's chosen mode).
        // Any `<advanced/>` from either prior fallback is preserved
        // on the new one.
        let extensions_xml = "<extensions xmlns='urn:xmpp:bookmarks:1'>\
                    <notify xmlns='urn:xmpp:notification-settings:1'>\
                        <always>\
                            <advanced xmlns='urn:xmpp:notification-settings:1'>\
                                <a xmlns='custom:a:1'/>\
                            </advanced>\
                        </always>\
                        <never>\
                            <advanced xmlns='urn:xmpp:notification-settings:1'>\
                                <b xmlns='custom:b:1'/>\
                            </advanced>\
                        </never>\
                    </notify>\
                    </extensions>";
        let extensions: Element = extensions_xml.parse().expect("valid xml");
        let merged = merge_notify_into_extensions(Some(&extensions), NotifyMode::OnMention, false);
        let notify = find_notify_in_extensions(&merged).expect("notify present");

        // Exactly one no-attrs fallback element on output.
        let fallback_count = notify
            .children()
            .filter(|c| {
                c.ns() == NS_NOTIFICATION_SETTINGS
                    && NotifyMode::from_wire_name(c.name()).is_some()
                    && c.attr("identity-category").is_none()
                    && c.attr("identity-type").is_none()
            })
            .count();
        assert_eq!(fallback_count, 1);

        // The single fallback carries exactly one `<advanced/>` folding
        // both salvaged foreign children — no rules dropped. XEP-0492
        // forbids more than one `<advanced/>` per setting (the server
        // validator rejects it), so the rich-payload normalizer (#719)
        // merges the two malformed-input blocks into one while
        // preserving every foreign child verbatim (§3 ¶1).
        let fallback = notify
            .children()
            .find(|c| c.name() == "on-mention" && c.attr("identity-category").is_none())
            .expect("new fallback");
        let advanced: Vec<&Element> = fallback
            .children()
            .filter(|c| c.is("advanced", NS_NOTIFICATION_SETTINGS))
            .collect();
        assert_eq!(advanced.len(), 1);
        let advanced = advanced[0];
        assert!(advanced.children().any(|c| c.ns() == "custom:a:1"));
        assert!(advanced.children().any(|c| c.ns() == "custom:b:1"));
    }

    #[test]
    fn merge_preserves_advanced_when_mode_unchanged() {
        // No-op merge: nothing to invalidate, the advanced rules
        // attached to the existing fallback stay intact.
        let extensions_xml = "<extensions xmlns='urn:xmpp:bookmarks:1'>\
                    <notify xmlns='urn:xmpp:notification-settings:1'>\
                        <always>\
                            <advanced xmlns='urn:xmpp:notification-settings:1'>\
                                <weekend xmlns='custom:other-client:1'/>\
                            </advanced>\
                        </always>\
                    </notify>\
                    </extensions>";
        let extensions: Element = extensions_xml.parse().expect("valid xml");
        let merged = merge_notify_into_extensions(Some(&extensions), NotifyMode::Always, false);
        let notify = find_notify_in_extensions(&merged).expect("notify present");
        let always_elem = notify
            .children()
            .find(|child| {
                child.name() == "always"
                    && child.attr("identity-category").is_none()
                    && child.attr("identity-type").is_none()
            })
            .expect("kept always present");
        let advanced = always_elem
            .get_child("advanced", NS_NOTIFICATION_SETTINGS)
            .expect("advanced preserved");
        assert!(advanced
            .children()
            .any(|child| child.name() == "weekend" && child.ns() == "custom:other-client:1"));
    }

    #[test]
    fn merge_emits_children_in_xsd_sequence_order() {
        // XEP-0492 v0.2.0 XSD declares <notify/> as a sequence of
        // `always` → `on-mention` → `never` (xeps/xep-0492.xml:177-180).
        // Input intentionally arrives out-of-order so we can assert
        // the sort.
        let extensions_xml = "<extensions xmlns='urn:xmpp:bookmarks:1'>\
                    <notify xmlns='urn:xmpp:notification-settings:1'>\
                        <never identity-category='client' identity-type='pc' />\
                        <always />\
                        <on-mention identity-category='client' identity-type='phone' />\
                    </notify>\
                    </extensions>";
        let extensions: Element = extensions_xml.parse().expect("valid xml");
        let merged = merge_notify_into_extensions(Some(&extensions), NotifyMode::Always, false);
        let notify = find_notify_in_extensions(&merged).expect("notify present");

        let names: Vec<&str> = notify.children().map(|c| c.name()).collect();
        // Output sequence: always (fallback) first, then on-mention
        // (identity-scoped phone), then never (identity-scoped pc).
        assert_eq!(names, vec!["always", "on-mention", "never"]);
    }

    #[test]
    fn merge_collapses_multiple_notify_siblings() {
        // Two `<notify/>` wrappers (a malformed but possible state):
        // dedupe and fold into a single output `<notify/>` per §3 ¶3.
        let extensions_xml = "<extensions xmlns='urn:xmpp:bookmarks:1'>\
                    <notify xmlns='urn:xmpp:notification-settings:1'>\
                        <always />\
                        <never identity-category='client' identity-type='phone' />\
                    </notify>\
                    <notify xmlns='urn:xmpp:notification-settings:1'>\
                        <always />\
                        <on-mention identity-category='client' />\
                    </notify>\
                    </extensions>";
        let extensions: Element = extensions_xml.parse().expect("valid xml");
        let merged = merge_notify_into_extensions(Some(&extensions), NotifyMode::OnMention, false);

        // Exactly one `<notify/>` wrapper.
        let notify_wrappers: Vec<_> = merged
            .children()
            .filter(|c| c.is("notify", NS_NOTIFICATION_SETTINGS))
            .collect();
        assert_eq!(notify_wrappers.len(), 1);

        let notify = notify_wrappers[0];
        // Fallback rewritten to on-mention; identity-scoped siblings
        // from both wrappers are preserved.
        assert_eq!(read_fallback_mode(notify), Some(NotifyMode::OnMention));
        assert!(notify.children().any(|c| {
            c.name() == "never"
                && c.attr("identity-category") == Some("client")
                && c.attr("identity-type") == Some("phone")
        }));
        assert!(notify.children().any(|c| {
            c.name() == "on-mention"
                && c.attr("identity-category") == Some("client")
                && c.attr("identity-type").is_none()
        }));

        // §3 ¶3 dedupe: the duplicate `<always />` fallback from the
        // second wrapper must not appear twice.
        let fallback_count = notify
            .children()
            .filter(|c| {
                c.ns() == NS_NOTIFICATION_SETTINGS
                    && NotifyMode::from_wire_name(c.name()).is_some()
                    && c.attr("identity-category").is_none()
                    && c.attr("identity-type").is_none()
            })
            .count();
        assert_eq!(fallback_count, 1);
    }

    #[test]
    fn merge_preserves_unrelated_extensions_siblings() {
        let extensions_xml = "<extensions xmlns='urn:xmpp:bookmarks:1'>\
                    <stuff xmlns='custom:other-spec:1'><x/></stuff>\
                    <notify xmlns='urn:xmpp:notification-settings:1'>\
                        <always />\
                    </notify>\
                    </extensions>";
        let extensions: Element = extensions_xml.parse().expect("valid xml");
        let merged = merge_notify_into_extensions(Some(&extensions), NotifyMode::OnMention, false);

        // Sibling extension preserved.
        assert!(merged
            .children()
            .any(|child| child.name() == "stuff" && child.ns() == "custom:other-spec:1"));

        // Notify rewritten.
        let notify = find_notify_in_extensions(&merged).expect("notify present");
        assert_eq!(read_fallback_mode(notify), Some(NotifyMode::OnMention));
    }

    #[test]
    fn merge_with_opt_in_round_trips_rich_payload() {
        // Tracer bullet — opting in writes the XEP-0492 §2.3
        // `<advanced><rich-payload xmlns='urn:waddle:push:rich:0'/>`
        // under the fallback, and the reader recovers it. This is the
        // exact wire shape the server's `parse_rich_payload_opt_in`
        // consumes (#719).
        let extensions = merge_notify_into_extensions(None, NotifyMode::Always, true);
        let notify = find_notify_in_extensions(&extensions).expect("notify present");
        assert!(read_rich_payload_opt_in(notify));

        // §2.3 shape: `<rich-payload/>` nests inside `<advanced/>`
        // inside the fallback, not directly on the fallback.
        let fallback = notify
            .children()
            .find(|c| c.name() == "always")
            .expect("fallback present");
        let advanced = fallback
            .get_child("advanced", NS_NOTIFICATION_SETTINGS)
            .expect("advanced present");
        assert!(advanced.has_child("rich-payload", NS_PUSH_RICH_PAYLOAD));
    }

    #[test]
    fn read_rich_payload_opt_in_false_without_advanced() {
        let notify: Element =
            "<notify xmlns='urn:xmpp:notification-settings:1'><always /></notify>"
                .parse()
                .expect("valid xml");
        assert!(!read_rich_payload_opt_in(&notify));
    }

    #[test]
    fn opt_out_removes_rich_payload_but_keeps_foreign_advanced() {
        // XEP-0492 §3 ¶1 — opting out drops OUR `<rich-payload/>` but
        // MUST NOT delete the foreign `<weekend/>` advanced setting.
        let extensions_xml = "<extensions xmlns='urn:xmpp:bookmarks:1'>\
                    <notify xmlns='urn:xmpp:notification-settings:1'>\
                        <always>\
                            <advanced xmlns='urn:xmpp:notification-settings:1'>\
                                <weekend xmlns='custom:other-client:1'/>\
                                <rich-payload xmlns='urn:waddle:push:rich:0'/>\
                            </advanced>\
                        </always>\
                    </notify>\
                    </extensions>";
        let extensions: Element = extensions_xml.parse().expect("valid xml");
        let merged = merge_notify_into_extensions(Some(&extensions), NotifyMode::Always, false);
        let notify = find_notify_in_extensions(&merged).expect("notify present");

        assert!(!read_rich_payload_opt_in(notify));
        let advanced = notify
            .children()
            .find(|c| c.name() == "always")
            .and_then(|f| f.get_child("advanced", NS_NOTIFICATION_SETTINGS))
            .expect("advanced kept for the foreign child");
        assert!(advanced
            .children()
            .any(|c| c.name() == "weekend" && c.ns() == "custom:other-client:1"));
        assert!(!advanced.has_child("rich-payload", NS_PUSH_RICH_PAYLOAD));
    }

    #[test]
    fn opt_out_drops_advanced_left_empty() {
        // §2.3 `<advanced/>` SHOULD NOT be empty — when our
        // `<rich-payload/>` was the only child, opting out removes the
        // now-empty `<advanced/>` entirely.
        let extensions_xml = "<extensions xmlns='urn:xmpp:bookmarks:1'>\
                    <notify xmlns='urn:xmpp:notification-settings:1'>\
                        <always>\
                            <advanced xmlns='urn:xmpp:notification-settings:1'>\
                                <rich-payload xmlns='urn:waddle:push:rich:0'/>\
                            </advanced>\
                        </always>\
                    </notify>\
                    </extensions>";
        let extensions: Element = extensions_xml.parse().expect("valid xml");
        let merged = merge_notify_into_extensions(Some(&extensions), NotifyMode::Always, false);
        let notify = find_notify_in_extensions(&merged).expect("notify present");
        let fallback = notify
            .children()
            .find(|c| c.name() == "always")
            .expect("fallback present");
        assert!(fallback
            .get_child("advanced", NS_NOTIFICATION_SETTINGS)
            .is_none());
    }

    #[test]
    fn opt_in_is_idempotent() {
        // Re-merging an already-opted-in setting MUST NOT accumulate a
        // second `<rich-payload/>` (§3 ¶3 — no duplicate elements).
        let once = merge_notify_into_extensions(None, NotifyMode::Always, true);
        let twice = merge_notify_into_extensions(Some(&once), NotifyMode::Always, true);
        let notify = find_notify_in_extensions(&twice).expect("notify present");
        let advanced = notify
            .children()
            .find(|c| c.name() == "always")
            .and_then(|f| f.get_child("advanced", NS_NOTIFICATION_SETTINGS))
            .expect("advanced present");
        let rich_count = advanced
            .children()
            .filter(|c| c.is("rich-payload", NS_PUSH_RICH_PAYLOAD))
            .count();
        assert_eq!(rich_count, 1);
    }

    #[test]
    fn opt_out_strips_rich_payload_from_identity_scoped_setting() {
        // Cross-client robustness: a `<rich-payload/>` marker placed on
        // an identity-scoped setting by another writer must also be
        // cleared on opt-out, since both the client reader and the
        // server `parse_rich_payload_opt_in` honor the marker on ANY
        // setting — otherwise the opt-out would be a silent no-op and
        // the UI checkbox would snap back on. The setting's foreign
        // `<weekend/>` rule and identity attrs are preserved (§3 ¶1).
        let extensions_xml = "<extensions xmlns='urn:xmpp:bookmarks:1'>\
                    <notify xmlns='urn:xmpp:notification-settings:1'>\
                        <always />\
                        <never identity-category='client' identity-type='pc'>\
                            <advanced xmlns='urn:xmpp:notification-settings:1'>\
                                <weekend xmlns='custom:other-client:1'/>\
                                <rich-payload xmlns='urn:waddle:push:rich:0'/>\
                            </advanced>\
                        </never>\
                    </notify>\
                    </extensions>";
        let extensions: Element = extensions_xml.parse().expect("valid xml");
        let merged = merge_notify_into_extensions(Some(&extensions), NotifyMode::Always, false);
        let notify = find_notify_in_extensions(&merged).expect("notify present");

        // The opt-in is now fully cleared — no setting carries it.
        assert!(!read_rich_payload_opt_in(notify));
        // The identity-scoped setting survives with its foreign rule
        // and attrs intact.
        let never = notify
            .children()
            .find(|c| {
                c.name() == "never"
                    && c.attr("identity-category") == Some("client")
                    && c.attr("identity-type") == Some("pc")
            })
            .expect("identity-scoped setting preserved");
        let advanced = never
            .get_child("advanced", NS_NOTIFICATION_SETTINGS)
            .expect("advanced kept for the foreign child");
        assert!(advanced
            .children()
            .any(|c| c.ns() == "custom:other-client:1"));
        assert!(!advanced.has_child("rich-payload", NS_PUSH_RICH_PAYLOAD));
    }

    #[test]
    fn opt_in_leaves_identity_scoped_setting_verbatim() {
        // On opt-in we record the marker on the fallback and do NOT
        // touch identity-scoped siblings — a foreign rule there is
        // preserved exactly.
        let extensions_xml = "<extensions xmlns='urn:xmpp:bookmarks:1'>\
                    <notify xmlns='urn:xmpp:notification-settings:1'>\
                        <always />\
                        <never identity-category='client'>\
                            <advanced xmlns='urn:xmpp:notification-settings:1'>\
                                <weekend xmlns='custom:other-client:1'/>\
                            </advanced>\
                        </never>\
                    </notify>\
                    </extensions>";
        let extensions: Element = extensions_xml.parse().expect("valid xml");
        let merged = merge_notify_into_extensions(Some(&extensions), NotifyMode::Always, true);
        let notify = find_notify_in_extensions(&merged).expect("notify present");
        assert!(read_rich_payload_opt_in(notify));
        let never = notify
            .children()
            .find(|c| c.name() == "never" && c.attr("identity-category") == Some("client"))
            .expect("identity-scoped setting preserved");
        let advanced = never
            .get_child("advanced", NS_NOTIFICATION_SETTINGS)
            .expect("foreign advanced preserved");
        assert!(advanced
            .children()
            .any(|c| c.ns() == "custom:other-client:1"));
    }

    #[test]
    fn opt_in_carries_through_mode_change() {
        // Changing the fallback name (always → never) while opting in
        // re-parents the opt-in under the new fallback (§3 ¶1).
        let always_opted = merge_notify_into_extensions(None, NotifyMode::Always, true);
        let now_never = merge_notify_into_extensions(Some(&always_opted), NotifyMode::Never, true);
        let notify = find_notify_in_extensions(&now_never).expect("notify present");
        assert_eq!(read_fallback_mode(notify), Some(NotifyMode::Never));
        assert!(read_rich_payload_opt_in(notify));
    }

    // --- DM carrier (#720) ---

    #[test]
    fn merge_notify_from_none_emits_single_fallback() {
        // The DM carrier's first-write path: no existing `<notify/>`.
        let notify = merge_notify(None, NotifyMode::Never, false);
        assert_eq!(notify.name(), "notify");
        assert_eq!(notify.ns(), NS_NOTIFICATION_SETTINGS);
        let children: Vec<_> = notify.children().collect();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].name(), "never");
        assert!(children[0].attr("identity-category").is_none());
        assert_eq!(read_fallback_mode(&notify), Some(NotifyMode::Never));
    }

    #[test]
    fn merge_notify_replaces_fallback_and_preserves_siblings() {
        // Existing `<notify/>` carried directly (DM shape — no
        // `<extensions/>` wrapper): the no-attrs fallback is rewritten
        // and the identity-scoped sibling survives verbatim.
        let existing: Element = "<notify xmlns='urn:xmpp:notification-settings:1'>\
                <never identity-category='client' identity-type='pc' />\
                <always />\
                </notify>"
            .parse()
            .expect("valid xml");
        let merged = merge_notify(Some(&existing), NotifyMode::OnMention, false);
        assert_eq!(read_fallback_mode(&merged), Some(NotifyMode::OnMention));
        assert!(merged.children().any(|c| {
            c.name() == "never"
                && c.attr("identity-category") == Some("client")
                && c.attr("identity-type") == Some("pc")
        }));
        let fallback_count = merged
            .children()
            .filter(|c| {
                c.ns() == NS_NOTIFICATION_SETTINGS
                    && NotifyMode::from_wire_name(c.name()).is_some()
                    && c.attr("identity-category").is_none()
                    && c.attr("identity-type").is_none()
            })
            .count();
        assert_eq!(fallback_count, 1);
    }

    #[test]
    fn merge_notify_round_trips_rich_payload_opt_in() {
        let merged = merge_notify(None, NotifyMode::Always, true);
        assert!(read_rich_payload_opt_in(&merged));
        // Re-merging is idempotent (no duplicate `<rich-payload/>`).
        let again = merge_notify(Some(&merged), NotifyMode::Always, true);
        let advanced = again
            .children()
            .find(|c| c.name() == "always")
            .and_then(|f| f.get_child("advanced", NS_NOTIFICATION_SETTINGS))
            .expect("advanced present");
        let rich_count = advanced
            .children()
            .filter(|c| c.is("rich-payload", NS_PUSH_RICH_PAYLOAD))
            .count();
        assert_eq!(rich_count, 1);
    }

    #[test]
    fn merge_notify_and_extensions_share_one_core() {
        // The refactor MUST keep MUC and DM byte-identical for the
        // `<notify/>` child: the `<extensions/>` wrapper's notify must
        // equal the bare `merge_notify` output for the same inputs.
        let extensions = merge_notify_into_extensions(None, NotifyMode::OnMention, true);
        let from_extensions = find_notify_in_extensions(&extensions).expect("notify present");
        let bare = merge_notify(None, NotifyMode::OnMention, true);
        assert_eq!(from_extensions, &bare);
    }

    #[test]
    fn dm_notify_is_default_true_for_plain_default() {
        // always + no opt-in + no foreign → default, retract the item.
        let notify = merge_notify(None, NotifyMode::Always, false);
        assert!(dm_notify_is_default(&notify, NotifyMode::Always));
    }

    #[test]
    fn dm_notify_is_default_false_for_on_mention() {
        let notify = merge_notify(None, NotifyMode::OnMention, false);
        assert!(!dm_notify_is_default(&notify, NotifyMode::Always));
    }

    #[test]
    fn dm_notify_is_default_false_for_never() {
        let notify = merge_notify(None, NotifyMode::Never, false);
        assert!(!dm_notify_is_default(&notify, NotifyMode::Always));
    }

    #[test]
    fn dm_notify_is_default_false_with_rich_opt_in() {
        // always + opt-in → an override (#719), keep the item.
        let notify = merge_notify(None, NotifyMode::Always, true);
        assert!(!dm_notify_is_default(&notify, NotifyMode::Always));
    }

    #[test]
    fn dm_notify_is_default_false_with_foreign_advanced() {
        // always fallback but a foreign `<advanced/>` rule another
        // client wrote → override, keep the item (§3 ¶1).
        let notify: Element = "<notify xmlns='urn:xmpp:notification-settings:1'>\
                <always>\
                    <advanced xmlns='urn:xmpp:notification-settings:1'>\
                        <weekend xmlns='custom:other-client:1'/>\
                    </advanced>\
                </always>\
                </notify>"
            .parse()
            .expect("valid xml");
        assert!(!dm_notify_is_default(&notify, NotifyMode::Always));
    }

    #[test]
    fn dm_notify_is_default_false_with_identity_scoped_sibling() {
        // always fallback but a foreign identity-scoped sibling →
        // override, keep the item.
        let notify: Element = "<notify xmlns='urn:xmpp:notification-settings:1'>\
                <always />\
                <never identity-category='client' identity-type='phone' />\
                </notify>"
            .parse()
            .expect("valid xml");
        assert!(!dm_notify_is_default(&notify, NotifyMode::Always));
    }

    #[test]
    fn dm_notify_is_default_false_without_fallback() {
        // No fallback child at all — only an identity-scoped sibling.
        // The siblings are foreign state, so this is NOT the bare
        // default; the item must persist.
        let notify: Element = "<notify xmlns='urn:xmpp:notification-settings:1'>\
                <never identity-category='client' />\
                </notify>"
            .parse()
            .expect("valid xml");
        assert!(!dm_notify_is_default(&notify, NotifyMode::Always));
    }

    #[test]
    fn build_and_read_dm_bookmark_round_trip() {
        let notify = merge_notify(None, NotifyMode::Never, false);
        let payload = build_dm_bookmark_element(&notify);
        assert_eq!(payload.name(), "dm-bookmark");
        assert_eq!(payload.ns(), NS_WADDLE_DM_BOOKMARKS);
        let recovered = read_dm_bookmark_notify(&payload).expect("notify present");
        assert_eq!(read_fallback_mode(recovered), Some(NotifyMode::Never));
        // The hosted `<notify/>` is byte-identical to the input —
        // Waddle hosts XEP-0492, it does not fork it.
        assert_eq!(recovered, &notify);
    }

    #[test]
    fn read_dm_bookmark_notify_returns_none_when_absent() {
        let payload = Element::builder("dm-bookmark", NS_WADDLE_DM_BOOKMARKS).build();
        assert!(read_dm_bookmark_notify(&payload).is_none());
    }

    #[test]
    fn ns_waddle_dm_bookmarks_is_byte_stable() {
        // Pins the wire value byte-compatible with the server module
        // `waddle_xmpp::xep::xep_waddle_dm_bookmarks` and the core
        // constant `waddle_xmpp_core::pubsub::pep`.
        assert_eq!(NS_WADDLE_DM_BOOKMARKS, "urn:waddle:dm-bookmarks:0");
    }

    #[test]
    fn build_fetch_dm_bookmarks_iq_targets_dm_node_without_to() {
        let iq = build_fetch_dm_bookmarks_iq("req-dm-fetch");
        assert_eq!(iq.attr("type"), Some("get"));
        // XEP-0163 §3.5 — omit `to=` so the server routes to the
        // account's own PEP service.
        assert!(iq.attr("to").is_none());
        let items = iq
            .get_child("pubsub", NS_PUBSUB)
            .and_then(|p| p.get_child("items", NS_PUBSUB))
            .expect("items present");
        assert_eq!(items.attr("node"), Some(NS_WADDLE_DM_BOOKMARKS));
    }

    #[test]
    fn build_publish_dm_bookmark_iq_uses_jid_id_and_hosts_notify() {
        let jid: BareJid = "bob@example.com".parse().expect("valid jid");
        let notify = merge_notify(None, NotifyMode::Never, false);
        let iq = build_publish_dm_bookmark_iq(&jid, &notify, "req-dm-pub");
        assert_eq!(iq.attr("type"), Some("set"));
        assert!(iq.attr("to").is_none());

        let publish = iq
            .get_child("pubsub", NS_PUBSUB)
            .and_then(|p| p.get_child("publish", NS_PUBSUB))
            .expect("publish present");
        assert_eq!(publish.attr("node"), Some(NS_WADDLE_DM_BOOKMARKS));
        let item = publish.get_child("item", NS_PUBSUB).expect("item present");
        // Item id MUST be the contact's bare JID.
        assert_eq!(item.attr("id"), Some("bob@example.com"));
        // Payload is `<dm-bookmark>` directly hosting the `<notify/>`.
        let payload = item
            .get_child("dm-bookmark", NS_WADDLE_DM_BOOKMARKS)
            .expect("dm-bookmark payload present");
        let hosted = read_dm_bookmark_notify(payload).expect("notify hosted");
        assert_eq!(read_fallback_mode(hosted), Some(NotifyMode::Never));
    }

    #[test]
    fn build_publish_dm_bookmark_iq_pins_canonical_publish_options() {
        let jid: BareJid = "bob@example.com".parse().expect("valid jid");
        let notify = merge_notify(None, NotifyMode::Never, false);
        let iq = build_publish_dm_bookmark_iq(&jid, &notify, "req-dm-pub-opts");
        let form = iq
            .get_child("pubsub", NS_PUBSUB)
            .and_then(|p| p.get_child("publish-options", NS_PUBSUB))
            .and_then(|po| po.get_child("x", "jabber:x:data"))
            .expect("publish-options form present");
        assert_eq!(form.attr("type"), Some("submit"));
        let value_of = |var: &str| -> Option<String> {
            form.children()
                .find(|child| child.attr("var") == Some(var))
                .and_then(|field| field.get_child("value", "jabber:x:data"))
                .map(|value| value.text())
        };
        assert_eq!(
            value_of("FORM_TYPE").as_deref(),
            Some("http://jabber.org/protocol/pubsub#publish-options"),
        );
        assert_eq!(value_of("pubsub#persist_items").as_deref(), Some("true"));
        assert_eq!(value_of("pubsub#max_items").as_deref(), Some("max"));
        assert_eq!(
            value_of("pubsub#send_last_published_item").as_deref(),
            Some("never"),
        );
        assert_eq!(
            value_of("pubsub#access_model").as_deref(),
            Some("whitelist"),
        );
    }

    #[test]
    fn build_retract_dm_bookmark_iq_retracts_jid_item() {
        let jid: BareJid = "bob@example.com".parse().expect("valid jid");
        let iq = build_retract_dm_bookmark_iq(&jid, "req-dm-retract");
        assert_eq!(iq.attr("type"), Some("set"));
        assert!(iq.attr("to").is_none());
        let retract = iq
            .get_child("pubsub", NS_PUBSUB)
            .and_then(|p| p.get_child("retract", NS_PUBSUB))
            .expect("retract present");
        assert_eq!(retract.attr("node"), Some(NS_WADDLE_DM_BOOKMARKS));
        assert_eq!(retract.attr("notify"), Some("true"));
        let item = retract.get_child("item", NS_PUBSUB).expect("item present");
        assert_eq!(item.attr("id"), Some("bob@example.com"));
    }
}

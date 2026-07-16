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

use minidom::Element;

/// XEP-0492 namespace.
pub const NS_NOTIFICATION_SETTINGS: &str = "urn:xmpp:notification-settings:1";

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

/// Surfaced XEP-0492 view of one conversation's notification state:
/// the fallback mode (if any) plus the Waddle rich-payload opt-in
/// (#719). This is the read-side companion to the merge helpers —
/// binding crates (WASM, UniFFI) map it onto their boundary shapes so
/// the sibling-scanning rules live in exactly one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotifySettingsSurface {
    /// XEP-0492 fallback setting (no `identity-*` attrs). `None` means
    /// no fallback child is present; the caller resolves against
    /// [`ConversationKind::default_notify_mode`] (§3).
    pub fallback_mode: Option<NotifyMode>,
    /// Waddle rich XEP-0357 push-summary opt-in carried in the
    /// XEP-0492 §2.3 `<advanced/>` block (#719). `false` is the
    /// default / absence-of-marker case.
    pub rich_payload_opt_in: bool,
}

/// Surface the XEP-0492 notification state from a XEP-0402 bookmark's
/// extension children.
///
/// A bookmark may (malformed-but-possible, folded by the merge code)
/// carry more than one `<notify/>` sibling, and a `<notify/>` may hold
/// only identity-scoped settings with no fallback. Scans ALL notify
/// children rather than stopping at the first: takes the first
/// fallback mode found, and treats the opt-in as set if ANY notify
/// sibling carries the rich-payload marker — matching how both
/// [`read_fallback_mode`] and the server's `parse_rich_payload_opt_in`
/// resolve across siblings. Round-2 PR #804 review (Codex P2). Pure.
pub fn surface_bookmark_notify(extensions: &[Element]) -> NotifySettingsSurface {
    let notifies = || {
        extensions
            .iter()
            .filter(|ext| ext.is("notify", NS_NOTIFICATION_SETTINGS))
    };
    NotifySettingsSurface {
        fallback_mode: notifies().find_map(read_fallback_mode),
        rich_payload_opt_in: notifies().any(read_rich_payload_opt_in),
    }
}

/// Surface the XEP-0492 notification state from a single hosted
/// `<notify/>` — the DM-bookmark carrier shape (issue #720), which
/// hosts exactly one `<notify/>` per contact. Pure.
pub fn surface_dm_notify(notify: &Element) -> NotifySettingsSurface {
    NotifySettingsSurface {
        fallback_mode: read_fallback_mode(notify),
        rich_payload_opt_in: read_rich_payload_opt_in(notify),
    }
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

/// Gather the children of one `<notify/>` element into `out`,
/// de-duplicating only XEP-0492 setting elements with identical
/// `(name, ns, identity-*)` per §3 ¶3. Foreign direct children are
/// preserved verbatim, including same-name siblings with different
/// attributes or contents, because the XEP de-dupe rule does not
/// apply to non-setting extension elements.
fn gather_notify_settings(notify: &Element, out: &mut Vec<Element>) {
    for setting in notify.children() {
        let is_xep_setting = setting.ns() == NS_NOTIFICATION_SETTINGS
            && NotifyMode::from_wire_name(setting.name()).is_some();
        if is_xep_setting && out.iter().any(|prior| settings_equivalent(prior, setting)) {
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
            // ours to manage. Strip it from identity-scoped settings in
            // both directions and record the current user opt-in only on
            // the fallback, so readers do not report stale markers from
            // another scoped sibling.
            identity_scoped.push(strip_rich_payload_from_setting(child));
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

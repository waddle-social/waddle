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
/// The function is pure — it takes a borrowed `extensions` element
/// and returns an owned new element.
pub fn merge_notify_into_extensions(extensions: Option<&Element>, mode: NotifyMode) -> Element {
    let mut builder = Element::builder("extensions", crate::pep::NS_BOOKMARKS);
    let mut notify_setting_children: Vec<Element> = Vec::new();

    if let Some(existing) = extensions {
        for child in existing.children() {
            if child.is("notify", NS_NOTIFICATION_SETTINGS) {
                for setting in child.children() {
                    // Dedupe identical (name, attrs) elements per §3 ¶3.
                    if notify_setting_children
                        .iter()
                        .any(|prior| settings_equivalent(prior, setting))
                    {
                        continue;
                    }
                    notify_setting_children.push(setting.clone());
                }
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
        .append(build_merged_notify_element(notify_setting_children, mode))
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
fn build_merged_notify_element(setting_children: Vec<Element>, mode: NotifyMode) -> Element {
    let mut identity_scoped: Vec<Element> = Vec::new();
    let mut preserved_advanced: Vec<Element> = Vec::new();
    let mut foreign: Vec<Element> = Vec::new();

    for child in setting_children {
        let is_setting = child.ns() == NS_NOTIFICATION_SETTINGS
            && NotifyMode::from_wire_name(child.name()).is_some();
        let has_identity_attr =
            child.attr("identity-category").is_some() || child.attr("identity-type").is_some();
        if is_setting && has_identity_attr {
            // Identity-scoped sibling — preserve verbatim.
            identity_scoped.push(child);
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
    // re-parenting any preserved `<advanced/>` children under it.
    let mut new_fallback = Element::builder(mode.as_wire_name(), NS_NOTIFICATION_SETTINGS);
    for advanced in preserved_advanced {
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
        let extensions = merge_notify_into_extensions(None, NotifyMode::OnMention);
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
        let merged = merge_notify_into_extensions(None, NotifyMode::Never);
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
        let merged = merge_notify_into_extensions(Some(&extensions), NotifyMode::OnMention);

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
        let merged = merge_notify_into_extensions(Some(&extensions), NotifyMode::Never);
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
        let merged = merge_notify_into_extensions(None, NotifyMode::Always);
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
        let merged = merge_notify_into_extensions(Some(&extensions), NotifyMode::OnMention);
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

        // The single fallback carries both salvaged advanced
        // children — no rules dropped.
        let fallback = notify
            .children()
            .find(|c| c.name() == "on-mention" && c.attr("identity-category").is_none())
            .expect("new fallback");
        let advanced: Vec<&Element> = fallback
            .children()
            .filter(|c| c.is("advanced", NS_NOTIFICATION_SETTINGS))
            .collect();
        // Two `<advanced/>` blocks are kept verbatim; we don't try
        // to merge their inner foreign children (they may not be
        // commutative).
        assert_eq!(advanced.len(), 2);
        assert!(advanced
            .iter()
            .any(|a| a.children().any(|c| c.ns() == "custom:a:1")));
        assert!(advanced
            .iter()
            .any(|a| a.children().any(|c| c.ns() == "custom:b:1")));
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
        let merged = merge_notify_into_extensions(Some(&extensions), NotifyMode::Always);
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
        let merged = merge_notify_into_extensions(Some(&extensions), NotifyMode::Always);
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
        let merged = merge_notify_into_extensions(Some(&extensions), NotifyMode::OnMention);

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
        let merged = merge_notify_into_extensions(Some(&extensions), NotifyMode::OnMention);

        // Sibling extension preserved.
        assert!(merged
            .children()
            .any(|child| child.name() == "stuff" && child.ns() == "custom:other-spec:1"));

        // Notify rewritten.
        let notify = find_notify_in_extensions(&merged).expect("notify present");
        assert_eq!(read_fallback_mode(notify), Some(NotifyMode::OnMention));
    }
}

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

/// Build a fresh `<notify/>` element containing only the requested
/// fallback child.
///
/// Used when the conversation has no existing `<extensions/>` content
/// — i.e. the merge helper has nothing to preserve.
pub fn build_notify_element(mode: NotifyMode) -> Element {
    Element::builder("notify", NS_NOTIFICATION_SETTINGS)
        .append(Element::builder(mode.as_wire_name(), NS_NOTIFICATION_SETTINGS).build())
        .build()
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

/// Find the XEP-0492 `<notify/>` child inside a XEP-0402
/// `<extensions/>` element. Returns `None` when no such child exists.
pub fn find_notify_in_extensions(extensions: &Element) -> Option<&Element> {
    extensions
        .children()
        .find(|child| child.is("notify", NS_NOTIFICATION_SETTINGS))
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
///   is **dropped**. The XEP-0492 §3 ¶4 "closest fallback" advice
///   means the writing client picked the parent element name to
///   encode its non-advanced fallback semantics; once the user
///   explicitly overrides that parent's name, the advanced rules
///   no longer reflect user intent. Re-publishing them under a
///   different parent would "alter" the setting per §3 ¶1, which
///   the spec forbids. Round-7 XEP reviewer P2 pinning.
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
fn build_merged_notify_element(setting_children: Vec<Element>, mode: NotifyMode) -> Element {
    let mut builder = Element::builder("notify", NS_NOTIFICATION_SETTINGS);
    let mut fallback_seen = false;
    for child in setting_children {
        let is_setting = child.ns() == NS_NOTIFICATION_SETTINGS
            && NotifyMode::from_wire_name(child.name()).is_some();
        let has_identity_attr =
            child.attr("identity-category").is_some() || child.attr("identity-type").is_some();
        if is_setting && !has_identity_attr {
            fallback_seen = true;
            let existing_mode = NotifyMode::from_wire_name(child.name());
            if existing_mode == Some(mode) {
                // No-op merge — preserve the existing element
                // verbatim, including any `<advanced/>` child.
                builder = builder.append(child);
            } else {
                // User explicitly overrode the fallback name; drop
                // the prior `<advanced/>` along with the old parent
                // (see merge_notify_into_extensions docstring).
                builder = builder.append(
                    Element::builder(mode.as_wire_name(), NS_NOTIFICATION_SETTINGS).build(),
                );
            }
        } else {
            // Identity-scoped sibling or non-setting child (e.g.
            // unknown foreign element) — preserve verbatim.
            builder = builder.append(child);
        }
    }
    if !fallback_seen {
        builder =
            builder.append(Element::builder(mode.as_wire_name(), NS_NOTIFICATION_SETTINGS).build());
    }
    builder.build()
}

/// Two setting elements are equivalent (for §3 ¶3 dedupe) when they
/// share the same name and the same identity attribute pair.
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

    #[test]
    fn build_notify_emits_single_fallback_child() {
        let elem = build_notify_element(NotifyMode::OnMention);
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
    fn merge_drops_advanced_when_fallback_name_changes() {
        // Round-7 XEP reviewer P2 — XEP-0492 §3 ¶4 picks the parent
        // element name to encode the non-advanced fallback semantics.
        // When the user explicitly changes the fallback name, the
        // advanced rules tied to the previous parent no longer reflect
        // user intent; re-publishing them under a different parent
        // would "alter" the setting per §3 ¶1. Drop is the only
        // spec-conformant move.
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

        // Fallback rewritten to never with NO advanced child.
        assert_eq!(read_fallback_mode(notify), Some(NotifyMode::Never));
        let never_elem = notify
            .children()
            .find(|child| {
                child.name() == "never"
                    && child.attr("identity-category").is_none()
                    && child.attr("identity-type").is_none()
            })
            .expect("rewritten never present");
        assert!(
            never_elem
                .get_child("advanced", NS_NOTIFICATION_SETTINGS)
                .is_none(),
            "advanced child must be dropped when the fallback name changes"
        );
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

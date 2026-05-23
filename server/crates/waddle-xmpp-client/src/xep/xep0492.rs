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
    notify
        .children()
        .filter(|child| child.ns() == NS_NOTIFICATION_SETTINGS)
        .find(|child| {
            NotifyMode::from_wire_name(child.name()).is_some()
                && child.attr("identity-category").is_none()
                && child.attr("identity-type").is_none()
        })
        .and_then(|child| NotifyMode::from_wire_name(child.name()))
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
/// * Inside the `<notify/>` element, identity-scoped siblings (those
///   carrying `identity-category` or `identity-type`) and any
///   `<advanced/>` children are preserved — XEP-0492 §3 first
///   paragraph forbids deleting `<advanced/>` settings we do not
///   support.
/// * The single existing fallback child (no `identity-*` attrs) is
///   replaced with one carrying the requested mode. XEP-0492 §3
///   forbids more than one element with the same name + attrs.
///
/// The function is pure — it takes a borrowed `extensions` element
/// and returns an owned new element.
pub fn merge_notify_into_extensions(extensions: Option<&Element>, mode: NotifyMode) -> Element {
    let mut builder = Element::builder("extensions", crate::pep::NS_BOOKMARKS);

    let mut notify_seen = false;
    if let Some(existing) = extensions {
        for child in existing.children() {
            if child.is("notify", NS_NOTIFICATION_SETTINGS) {
                notify_seen = true;
                builder = builder.append(merge_notify_child(child, mode));
            } else {
                builder = builder.append(child.clone());
            }
        }
    }

    if !notify_seen {
        builder = builder.append(build_notify_element(mode));
    }

    builder.build()
}

/// Merge a desired mode into a single `<notify/>` element. Identity-
/// scoped siblings (with `identity-category` or `identity-type`) and
/// any `<advanced/>` children inside them are preserved; the single
/// fallback child is replaced.
fn merge_notify_child(notify: &Element, mode: NotifyMode) -> Element {
    let mut builder = Element::builder("notify", NS_NOTIFICATION_SETTINGS);
    let mut fallback_seen = false;
    for child in notify.children() {
        let is_setting_child = child.ns() == NS_NOTIFICATION_SETTINGS
            && NotifyMode::from_wire_name(child.name()).is_some();
        let has_identity_attr =
            child.attr("identity-category").is_some() || child.attr("identity-type").is_some();
        if is_setting_child && !has_identity_attr {
            // Fallback child — replace with the requested mode but
            // preserve any `<advanced/>` children inside it (§3:
            // MUST NOT delete or alter `<advanced/>` settings we do
            // not support).
            fallback_seen = true;
            let mut replacement = Element::builder(mode.as_wire_name(), NS_NOTIFICATION_SETTINGS);
            for inner in child.children() {
                if inner.is("advanced", NS_NOTIFICATION_SETTINGS) {
                    replacement = replacement.append(inner.clone());
                }
            }
            builder = builder.append(replacement.build());
        } else {
            builder = builder.append(child.clone());
        }
    }
    if !fallback_seen {
        builder =
            builder.append(Element::builder(mode.as_wire_name(), NS_NOTIFICATION_SETTINGS).build());
    }
    builder.build()
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
    fn merge_preserves_advanced_foreign_children() {
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

        // Fallback rewritten to never.
        assert_eq!(read_fallback_mode(notify), Some(NotifyMode::Never));

        // Advanced foreign extension carried over inside the new
        // fallback child.
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
            .expect("advanced preserved");
        assert!(advanced
            .children()
            .any(|child| child.name() == "weekend" && child.ns() == "custom:other-client:1"));
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

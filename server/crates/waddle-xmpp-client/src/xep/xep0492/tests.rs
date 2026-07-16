use super::*;
use crate::pep::NS_PUBSUB;
use jid::BareJid;
use minidom::Element;

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
fn merge_preserves_duplicate_foreign_notify_children_verbatim() {
    // XEP-0492 §3 ¶3 de-duplicates setting elements, not arbitrary
    // foreign extension children. Same-name foreign children with
    // different attrs/content must survive exactly as written.
    let extensions_xml = "<extensions xmlns='urn:xmpp:bookmarks:1'>\
                    <notify xmlns='urn:xmpp:notification-settings:1'>\
                        <always />\
                        <rule xmlns='custom:notify:rules:1' id='a'><value xmlns='custom:notify:rules:1'>one</value></rule>\
                        <rule xmlns='custom:notify:rules:1' id='b'><value xmlns='custom:notify:rules:1'>two</value></rule>\
                    </notify>\
                    </extensions>";
    let extensions: Element = extensions_xml.parse().expect("valid xml");
    let merged = merge_notify_into_extensions(Some(&extensions), NotifyMode::Never, false);
    let notify = find_notify_in_extensions(&merged).expect("notify present");
    let rules: Vec<&Element> = notify
        .children()
        .filter(|child| child.is("rule", "custom:notify:rules:1"))
        .collect();

    assert_eq!(rules.len(), 2);
    assert_eq!(rules[0].attr("id"), Some("a"));
    assert_eq!(
        rules[0]
            .get_child("value", "custom:notify:rules:1")
            .map(|value| value.text())
            .as_deref(),
        Some("one")
    );
    assert_eq!(rules[1].attr("id"), Some("b"));
    assert_eq!(
        rules[1]
            .get_child("value", "custom:notify:rules:1")
            .map(|value| value.text())
            .as_deref(),
        Some("two")
    );
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
    let notify: Element = "<notify xmlns='urn:xmpp:notification-settings:1'><always /></notify>"
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
fn opt_in_preserves_identity_scoped_foreign_rules_but_centralizes_rich_payload() {
    // On opt-in we record the Waddle marker on the fallback. Foreign
    // identity-scoped rules are preserved, but a stale Waddle-owned
    // marker on that scoped sibling is removed so the fallback is the
    // single owner of the current account opt-in state.
    let extensions_xml = "<extensions xmlns='urn:xmpp:bookmarks:1'>\
                    <notify xmlns='urn:xmpp:notification-settings:1'>\
                        <always />\
                        <never identity-category='client'>\
                            <advanced xmlns='urn:xmpp:notification-settings:1'>\
                                <rich-payload xmlns='urn:waddle:push:rich:0'/>\
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
    assert!(!advanced.has_child("rich-payload", NS_PUSH_RICH_PAYLOAD));

    let fallback = notify
        .children()
        .find(|c| c.name() == "always" && c.attr("identity-category").is_none())
        .expect("fallback preserved");
    let fallback_advanced = fallback
        .get_child("advanced", NS_NOTIFICATION_SETTINGS)
        .expect("fallback advanced owns rich marker");
    assert!(fallback_advanced.has_child("rich-payload", NS_PUSH_RICH_PAYLOAD));
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
fn dm_notify_is_default_false_with_foreign_direct_child() {
    // always fallback PLUS a foreign element directly under
    // `<notify/>` (not a recognized XEP-0492 setting). It is unknown
    // state — be conservative and keep the item rather than retract
    // it, which would drop the foreign child (Copilot review).
    let notify: Element = "<notify xmlns='urn:xmpp:notification-settings:1'>\
                <always />\
                <future-thing xmlns='custom:other-client:2'/>\
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
    // Pin the client's own copy against the live core constant (the
    // canonical node/namespace string) — NOT a hardcoded literal — so
    // a core bump (e.g. to `urn:waddle:dm-bookmarks:1`) that forgets
    // this client copy fails CI here rather than silently diverging
    // the client's wire value from the server's (greptile review).
    assert_eq!(
        NS_WADDLE_DM_BOOKMARKS,
        waddle_xmpp_core::pubsub::PEP_NODE_WADDLE_DM_BOOKMARKS
    );
}

#[test]
fn dm_bookmark_max_items_matches_server_node_default() {
    // The publish-options request MUST pin the same finite cap the
    // server node defaults to, so the requested config matches the
    // node the server creates on first publish. Anti-DoS parity with
    // the XEP-0402 bookmarks node — guard against future drift between
    // the client constant and the server core constant.
    assert_eq!(
        DM_BOOKMARK_MAX_ITEMS,
        waddle_xmpp_core::pubsub::PEP_BOOKMARK_MAX_ITEMS,
    );
    assert_ne!(DM_BOOKMARK_MAX_ITEMS, u32::MAX);
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
    // The DM node requests a FINITE cap (anti-DoS parity with the
    // XEP-0402 bookmarks node) rather than `max` — an unbounded node
    // disables server-side eviction. The value MUST match the server
    // node default `PEP_BOOKMARK_MAX_ITEMS`.
    assert_eq!(
        value_of("pubsub#max_items").as_deref(),
        Some(DM_BOOKMARK_MAX_ITEMS.to_string().as_str()),
    );
    assert_ne!(
        value_of("pubsub#max_items").as_deref(),
        Some("max"),
        "DM node must request a finite cap so eviction stays enabled"
    );
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

#[test]
fn surface_bookmark_notify_reads_fallback_and_rich_opt_in() {
    // #719 — the surfaced state exposes the rich-payload opt-in from
    // the fallback's <advanced/> alongside the fallback mode itself.
    let extensions: Element = "<extensions xmlns='urn:xmpp:bookmarks:1'>\
                <notify xmlns='urn:xmpp:notification-settings:1'>\
                    <always>\
                        <advanced xmlns='urn:xmpp:notification-settings:1'>\
                            <rich-payload xmlns='urn:waddle:push:rich:0'/>\
                        </advanced>\
                    </always>\
                </notify>\
            </extensions>"
        .parse()
        .expect("valid extensions");
    let children: Vec<Element> = extensions.children().cloned().collect();
    let surfaced = surface_bookmark_notify(&children);
    assert_eq!(surfaced.fallback_mode, Some(NotifyMode::Always));
    assert!(surfaced.rich_payload_opt_in);
}

#[test]
fn surface_bookmark_notify_opt_in_defaults_off_without_advanced() {
    let extensions: Element = "<extensions xmlns='urn:xmpp:bookmarks:1'>\
                <notify xmlns='urn:xmpp:notification-settings:1'><never /></notify>\
            </extensions>"
        .parse()
        .expect("valid extensions");
    let children: Vec<Element> = extensions.children().cloned().collect();
    let surfaced = surface_bookmark_notify(&children);
    assert_eq!(surfaced.fallback_mode, Some(NotifyMode::Never));
    assert!(!surfaced.rich_payload_opt_in);
}

#[test]
fn surface_bookmark_notify_absent_notify_yields_none() {
    // No <notify/> extension at all — the caller resolves against the
    // XEP-0492 §3 conversation-kind default.
    let surfaced = surface_bookmark_notify(&[]);
    assert_eq!(surfaced.fallback_mode, None);
    assert!(!surfaced.rich_payload_opt_in);
}

#[test]
fn surface_bookmark_notify_scans_past_fallbackless_sibling() {
    // Malformed-but-possible state the merge code explicitly folds:
    // multiple <notify/> siblings. The first carries only an
    // identity-scoped setting (no fallback, no rich marker); the
    // user's real fallback + rich opt-in live in the second. ALL
    // notify siblings must be scanned, not just the first.
    let extensions: Element = "<extensions xmlns='urn:xmpp:bookmarks:1'>\
                <notify xmlns='urn:xmpp:notification-settings:1'>\
                    <never identity-category='client' identity-type='pc' />\
                </notify>\
                <notify xmlns='urn:xmpp:notification-settings:1'>\
                    <always>\
                        <advanced xmlns='urn:xmpp:notification-settings:1'>\
                            <rich-payload xmlns='urn:waddle:push:rich:0'/>\
                        </advanced>\
                    </always>\
                </notify>\
            </extensions>"
        .parse()
        .expect("valid extensions");
    let children: Vec<Element> = extensions.children().cloned().collect();
    let surfaced = surface_bookmark_notify(&children);
    assert_eq!(surfaced.fallback_mode, Some(NotifyMode::Always));
    assert!(surfaced.rich_payload_opt_in);
}

#[test]
fn surface_dm_notify_reads_hosted_notify() {
    let notify: Element = "<notify xmlns='urn:xmpp:notification-settings:1'>\
                <on-mention>\
                    <advanced xmlns='urn:xmpp:notification-settings:1'>\
                        <rich-payload xmlns='urn:waddle:push:rich:0'/>\
                    </advanced>\
                </on-mention>\
            </notify>"
        .parse()
        .expect("valid notify");
    let surfaced = surface_dm_notify(&notify);
    assert_eq!(surfaced.fallback_mode, Some(NotifyMode::OnMention));
    assert!(surfaced.rich_payload_opt_in);
}

#[test]
fn surface_dm_notify_identity_only_yields_no_fallback() {
    // A <notify/> holding only identity-scoped siblings has no
    // fallback; the caller resolves against the §3 direct-chat
    // default (`always`).
    let notify: Element = "<notify xmlns='urn:xmpp:notification-settings:1'>\
                <never identity-category='client' identity-type='pc' />\
            </notify>"
        .parse()
        .expect("valid notify");
    let surfaced = surface_dm_notify(&notify);
    assert_eq!(surfaced.fallback_mode, None);
    assert!(!surfaced.rich_payload_opt_in);
}

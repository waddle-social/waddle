use super::*;

#[test]
fn fetch_threads_options_build_filtered_iq() {
    let opts = WaddleFetchThreadsOptions {
        page_size: Some(50),
        after_cursor: Some("CUR".to_string()),
        status: Some(WaddleThreadStatusFilter::Unread),
        active_since: Some("2026-05-19T00:00:00Z".to_string()),
        channel: Some("chat@muc.waddle.chat".to_string()),
        search: Some(" notifications ".to_string()),
        sort: Some(WaddleThreadSort::Replies),
    };

    let query = fetch_threads_query_from_options(&opts).expect("valid threads query");
    let iq = build_fetch_threads_iq("threads-test", &query);
    let query_el = iq
        .get_child("query", waddle_xmpp_client::xep::threads::NS_THREADS)
        .expect("threads query");

    assert_eq!(query_el.attr("status"), Some("unread"));
    assert_eq!(query_el.attr("active-since"), Some("2026-05-19T00:00:00Z"));
    assert_eq!(query_el.attr("channel"), Some("chat@muc.waddle.chat"));
    assert_eq!(query_el.attr("search"), Some("notifications"));
    assert_eq!(query_el.attr("sort"), Some("replies"));

    let set = query_el
        .get_child("set", "http://jabber.org/protocol/rsm")
        .expect("rsm set");
    assert_eq!(
        set.get_child("max", "http://jabber.org/protocol/rsm")
            .map(|el| el.text()),
        Some("50".to_string())
    );
    assert_eq!(
        set.get_child("after", "http://jabber.org/protocol/rsm")
            .map(|el| el.text()),
        Some("CUR".to_string())
    );
}

#[test]
fn verify_iq_from_accepts_exact_match() {
    assert!(verify_iq_from_matches_query(Some("push.example.com"), "push.example.com").is_ok());
}

#[test]
fn verify_iq_from_rejects_absent_attr() {
    // RFC 6120 §8.1.2.1's absent-from permission only applies to
    // replies from the bound user's own server. The Push Service is
    // a separate XEP-0114 component, so absent-from is a server
    // violation we MUST NOT silently accept — a compromised C2S
    // could strip `from` instead of spoofing it.
    let err = verify_iq_from_matches_query(None, "push.example.com").unwrap_err();
    assert!(err.contains("push.example.com"));
    assert!(err.contains("no `from`"));
}

#[test]
fn parse_dm_bookmark_payloads_skips_domain_only_item_id() {
    // A domain-only id (no localpart) is not a valid DM contact JID
    // — it must be skipped so the chat never surfaces an impossible
    // DM entry (Copilot review). A well-formed sibling still parses.
    let iq: Element = "<iq xmlns='jabber:client'>\
                <pubsub xmlns='http://jabber.org/protocol/pubsub'>\
                    <items node='urn:waddle:dm-bookmarks:0'>\
                        <item id='bob@example.com'>\
                            <dm-bookmark xmlns='urn:waddle:dm-bookmarks:0'>\
                                <notify xmlns='urn:xmpp:notification-settings:1'><never/></notify>\
                            </dm-bookmark>\
                        </item>\
                        <item id='example.com'>\
                            <dm-bookmark xmlns='urn:waddle:dm-bookmarks:0'>\
                                <notify xmlns='urn:xmpp:notification-settings:1'><never/></notify>\
                            </dm-bookmark>\
                        </item>\
                    </items>\
                </pubsub>\
            </iq>"
        .parse()
        .expect("valid iq");
    let parsed = parse_dm_bookmarks_response(&iq);
    assert_eq!(parsed.len(), 1, "domain-only id must be skipped");
    assert_eq!(parsed[0].jid, "bob@example.com");
}

#[test]
fn verify_iq_from_rejects_service_jid_with_resource() {
    // Push Service components don't carry resources (XEP-0114).
    // Accepting `push.example.com/anything` would be needlessly
    // permissive and wouldn't help any real-world deployment.
    let err = verify_iq_from_matches_query(Some("push.example.com/resource"), "push.example.com")
        .unwrap_err();
    assert!(err.contains("push.example.com/resource"));
}

#[test]
fn verify_iq_from_accepts_case_variation_on_domain() {
    // RFC 6122 §2.4: domainpart comparison is case-insensitive.
    assert!(verify_iq_from_matches_query(Some("Push.Example.COM"), "push.example.com").is_ok());
}

#[test]
fn verify_iq_from_rejects_unrelated_jid() {
    let err = verify_iq_from_matches_query(Some("attacker.tld"), "push.example.com").unwrap_err();
    assert!(err.contains("attacker.tld"));
    assert!(err.contains("push.example.com"));
}

#[test]
fn verify_iq_from_rejects_prefix_substring_collision() {
    // `push.example.com` MUST NOT match `push.example.com.attacker.tld`
    // even via case-insensitive equality.
    assert!(verify_iq_from_matches_query(
        Some("push.example.com.attacker.tld"),
        "push.example.com",
    )
    .is_err());
}

#[test]
fn verify_iq_from_rejects_empty_from() {
    let err = verify_iq_from_matches_query(Some(""), "push.example.com").unwrap_err();
    assert!(err.contains("push.example.com"));
}

#[test]
fn surface_bookmark_reads_rich_payload_opt_in() {
    // #719 — the JS-facing item exposes the XEP-0492 §2.3
    // `<advanced><rich-payload xmlns='urn:waddle:push:rich:0'/>`
    // opt-in alongside the fallback notify mode.
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
    let item = BookmarkItem {
        jid: "room@muc.example.com".parse().expect("valid jid"),
        name: None,
        autojoin: false,
        nick: None,
        password: None,
        extensions: extensions.children().cloned().collect(),
    };
    let surfaced = surface_bookmark(item);
    assert!(surfaced.rich_payload_opt_in);
    assert_eq!(
        surfaced.notify_mode,
        Some(waddle_xmpp_client::xep::xep0492::NotifyMode::Always)
    );
}

#[test]
fn surface_bookmark_opt_in_defaults_off_without_advanced() {
    let extensions: Element = "<extensions xmlns='urn:xmpp:bookmarks:1'>\
                <notify xmlns='urn:xmpp:notification-settings:1'><never /></notify>\
            </extensions>"
        .parse()
        .expect("valid extensions");
    let item = BookmarkItem {
        jid: "room@muc.example.com".parse().expect("valid jid"),
        name: None,
        autojoin: false,
        nick: None,
        password: None,
        extensions: extensions.children().cloned().collect(),
    };
    let surfaced = surface_bookmark(item);
    assert!(!surfaced.rich_payload_opt_in);
}

#[test]
fn surface_bookmark_scans_past_fallbackless_notify_sibling() {
    // Malformed-but-possible state the merge code explicitly folds:
    // multiple <notify/> siblings. The first carries only an
    // identity-scoped setting (no fallback, no rich marker); the
    // user's real fallback + rich opt-in live in the second. We
    // must scan ALL notify siblings, not stop at the first.
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
    let item = BookmarkItem {
        jid: "room@muc.example.com".parse().expect("valid jid"),
        name: None,
        autojoin: false,
        nick: None,
        password: None,
        extensions: extensions.children().cloned().collect(),
    };
    let surfaced = surface_bookmark(item);
    assert_eq!(
        surfaced.notify_mode,
        Some(waddle_xmpp_client::xep::xep0492::NotifyMode::Always)
    );
    assert!(surfaced.rich_payload_opt_in);
}

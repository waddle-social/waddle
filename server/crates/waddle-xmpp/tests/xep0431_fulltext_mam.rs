//! XEP-0431: Full Text Search in MAM — dedicated conformance suite.
//!
//! XEP-0431 (Deferred) extends XEP-0313's MAM query form with a
//! `{urn:xmpp:fulltext:0}fulltext` field, letting clients search
//! the archive by keyword. Pinning the audit-level invariants:
//!
//! - the §"Discovering Support" namespace string `urn:xmpp:fulltext:0`
//!   and the prefixed-FORM-field identifier
//!   `{urn:xmpp:fulltext:0}fulltext`,
//! - disco advertisement on every MUC room configuration (the room
//!   archive is where full-text search has real surface area for
//!   Waddle),
//! - `MamSearchQuery` builder semantics — `is_empty()` correctly
//!   treats whitespace-only fulltext as a no-op so an accidental
//!   empty submit doesn't return the entire archive,
//! - `matches_fulltext` matches every term (AND semantics),
//!   case-insensitively, in any order — the predicate used by
//!   in-memory backends and migration helpers,
//! - `SearchResult::snippet` produces useful context windows
//!   without panicking on UTF-8 char boundaries (the search index
//!   is byte-keyed but message bodies are UTF-8).

use waddle_xmpp::disco::{muc_room_features, Feature};
use waddle_xmpp::xep::xep0431::{
    matches_fulltext, MamSearchQuery, SearchResult, FIELD_FULLTEXT, NS_FULLTEXT,
};

// ── §"Discovering Support" namespace + field identifiers ────────────

#[test]
fn xep0431_namespace_matches_spec() {
    // §"Discovering Support" pins the namespace URI. MAM
    // server-side dispatches on this string when parsing the
    // form's `var=` attribute (after the `{ns}local` prefix
    // unwrap).
    assert_eq!(NS_FULLTEXT, "urn:xmpp:fulltext:0");
}

#[test]
fn xep0431_field_identifier_uses_prefixed_form() {
    // §"Querying" example shows the field var as
    // `{urn:xmpp:fulltext:0}fulltext`. The prefixed-namespace
    // syntax is XEP-0004 §3.4's convention for disambiguating
    // form-field names across extension namespaces; without it,
    // the MAM dispatcher couldn't tell a XEP-0431 `fulltext`
    // field apart from any other extension's `fulltext`.
    assert_eq!(FIELD_FULLTEXT, "{urn:xmpp:fulltext:0}fulltext");
    // Sanity: the prefixed form really embeds the namespace.
    assert!(FIELD_FULLTEXT.contains(NS_FULLTEXT));
}

// ── §"Discovering Support" advertisement ────────────────────────────

#[test]
fn xep0431_advertised_on_every_room_configuration() {
    // §"Discovering Support" requires `urn:xmpp:fulltext:0` in
    // disco#info for any service that supports full-text MAM.
    // Waddle's room MAM backend supports it uniformly, so the
    // advert MUST survive every config cell — otherwise a client
    // would skip the feature based on a single room sample.
    let target = Feature::fulltext_mam();
    assert_eq!(target.0, NS_FULLTEXT);

    for persistent in [false, true] {
        for members_only in [false, true] {
            for moderated in [false, true] {
                for forum in [false, true] {
                    let feats = muc_room_features(
                        waddle_xmpp_core::disco::info::RoomPersistence::from_persistent_flag(
                            persistent,
                        ),
                        waddle_xmpp_core::disco::info::RoomMembership::from_members_only_flag(
                            members_only,
                        ),
                        waddle_xmpp_core::disco::info::RoomVisibility::Public,
                        waddle_xmpp_core::types::Moderation::from_moderated_flag(moderated),
                    );
                    assert!(
                        feats.iter().any(|f| f == &target),
                        "muc_room_features({persistent}, {members_only}, {moderated}, {forum}) \
                         MUST advertise `urn:xmpp:fulltext:0`"
                    );
                }
            }
        }
    }
}

// ── MamSearchQuery semantics ────────────────────────────────────────

#[test]
fn xep0431_query_is_empty_treats_whitespace_as_no_op() {
    // A whitespace-only fulltext value is the same as no value at
    // all — submitting it would degenerate the MAM search into
    // "return the entire archive." The helper MUST surface that
    // case as `is_empty()` so handlers can reject or fall through
    // to a normal MAM request.
    assert!(MamSearchQuery::new("").is_empty());
    assert!(MamSearchQuery::new("   ").is_empty());
    assert!(MamSearchQuery::new("\t\n").is_empty());
    assert!(!MamSearchQuery::new("real text").is_empty());
}

#[test]
fn xep0431_query_builder_chains_optional_filters() {
    let q = MamSearchQuery::new("urgent")
        .with_max(50)
        .with_jid("ops@conf.example.com");
    assert_eq!(q.fulltext, "urgent");
    assert_eq!(q.max, Some(50));
    assert_eq!(q.with.as_deref(), Some("ops@conf.example.com"));
}

// ── matches_fulltext: AND semantics, case-insensitive ───────────────

#[test]
fn xep0431_matches_fulltext_is_case_insensitive_and_term_anded() {
    // Multiple terms ⇒ all must appear (AND), not any (OR). OR
    // semantics would surface unrelated messages and break the
    // user-visible search expectation that `Hello World` finds
    // messages with BOTH words.
    assert!(matches_fulltext("Hello World", "hello"));
    assert!(matches_fulltext("Hello World", "WORLD"));
    assert!(matches_fulltext("The Quick Brown Fox", "quick fox"));
    assert!(matches_fulltext("The Quick Brown Fox", "fox quick"));
    assert!(!matches_fulltext("The Quick Brown Fox", "quick cat"));
    assert!(!matches_fulltext("Hello World", "foo"));
}

#[test]
fn xep0431_matches_fulltext_empty_query_matches_anything() {
    // An empty term-set is trivially satisfied (the AND-fold over
    // zero terms is true). This is the documented degenerate; the
    // `is_empty()` guard above is what handlers use to reject the
    // case at the request boundary.
    assert!(matches_fulltext("anything", ""));
    assert!(matches_fulltext("anything", "   "));
}

// ── SearchResult::snippet: UTF-8 safe context window ────────────────

#[test]
fn xep0431_snippet_with_match_in_middle_adds_leading_ellipsis() {
    // §"Results" doesn't mandate snippet formatting, but Waddle's
    // implementation prepends "..." when the match isn't at the
    // start. Test pins this rendering so a future change is a
    // deliberate UX decision rather than a silent shift.
    let r = SearchResult::new("1", "a", "The quick brown fox jumps over the lazy dog", "t");
    let snippet = r.snippet("fox", 10);
    assert!(snippet.contains("fox"));
    assert!(snippet.starts_with("..."));
}

#[test]
fn xep0431_snippet_match_at_start_skips_leading_ellipsis() {
    let r = SearchResult::new("1", "a", "Hello world this is a test", "t");
    let snippet = r.snippet("Hello", 10);
    assert!(snippet.starts_with("Hello"));
}

#[test]
fn xep0431_snippet_with_no_match_returns_truncated_body() {
    let r = SearchResult::new("1", "a", "Short message", "t");
    let snippet = r.snippet("xyz", 20);
    assert_eq!(snippet, "Short message");
}

#[test]
fn xep0431_snippet_is_unicode_safe_around_multibyte_chars() {
    // The context-window slicing operates on byte offsets but
    // needs to land on UTF-8 char boundaries. A multi-byte char
    // sitting just inside the [start..end] window would otherwise
    // panic. Pin this against a body with emoji + accented chars.
    let r = SearchResult::new(
        "1",
        "a",
        "café ☕ for romeo plus juliet at the balcony 🌹",
        "t",
    );
    // Pick a small window so the snippet boundaries are forced to
    // navigate multi-byte characters.
    let snippet = r.snippet("romeo", 5);
    assert!(snippet.contains("romeo"), "match should appear: {snippet}");
}

#[test]
fn xep0431_snippet_is_case_insensitive_for_match_detection() {
    // The case-insensitive match must apply to *detection* — the
    // snippet itself preserves the body's original casing so the
    // UI doesn't lowercase the user's content unexpectedly.
    let r = SearchResult::new("1", "a", "Hello WORLD friend", "t");
    let snippet = r.snippet("world", 5);
    assert!(
        snippet.contains("WORLD"),
        "snippet preserves original case: {snippet}"
    );
}

// ── SearchResult builder ────────────────────────────────────────────

#[test]
fn xep0431_search_result_carries_optional_room_jid_for_muc_results() {
    // MUC search results include the room JID so the client can
    // jump back to the right channel; 1:1 search omits it.
    let muc = SearchResult::new("arc-1", "alice", "Hello", "t").with_room("room@muc.example.com");
    assert_eq!(muc.room_jid.as_deref(), Some("room@muc.example.com"));

    let direct = SearchResult::new("arc-2", "bob", "Hi", "t");
    assert!(
        direct.room_jid.is_none(),
        "direct-message result has no room JID"
    );
}

// ── §"Querying": the sqlx-backed archive honours the search intent ──

mod sqlx_archive {
    use chrono::Utc;
    use jid::{BareJid, Jid};
    use waddle_xmpp::mam::{ArchivedMessage, MamArchiveKind, MamQuery, MamStorage, SqlxMamStorage};
    use waddle_xmpp_core::mam::RichText;
    use waddle_xmpp_core::xep0359::StanzaId;
    use xmpp_parsers::message::MessageType;

    const ROOM: &str = "room@conference.example.com";

    fn room() -> BareJid {
        ROOM.parse().expect("room jid")
    }

    fn row(id: &str, body: &str) -> ArchivedMessage {
        let room_jid: Jid = ROOM.parse().expect("room jid");
        ArchivedMessage {
            ordinal: None,
            id: id.to_string(),
            timestamp: Utc::now(),
            from: format!("{ROOM}/alice").parse().expect("occupant jid"),
            to: room_jid.clone(),
            body: Some(body.to_string()),
            stanza_id: Some(StanzaId::new(format!("wire-{id}"), room_jid)),
            thread: None,
            reply: None,
            origin_id: None,
            message_type: MessageType::Groupchat,
            stanza_xml: None,
            rich: None,
            nickname_generation: Some(0),
        }
    }

    async fn search(bodies: &[&str], needle: &str) -> Vec<String> {
        let storage = SqlxMamStorage::open_in_memory().await.expect("open sqlite");
        for (index, body) in bodies.iter().enumerate() {
            storage
                .store_message(&room(), &row(&format!("m{index}"), body))
                .await
                .expect("store");
        }
        let result = storage
            .query_messages(
                &room(),
                MamArchiveKind::Room,
                &MamQuery {
                    fulltext: RichText::try_new(needle),
                    ..Default::default()
                },
            )
            .await
            .expect("query");
        result
            .messages
            .into_iter()
            .map(|m| m.body.unwrap_or_default())
            .collect()
    }

    #[tokio::test]
    async fn percent_in_term_is_literal() {
        // "the intent is that a reasonable full-text search is performed
        // based on the input": `100%` must not act as a wildcard.
        let hits = search(&["1000 apples", "100% done"], "100%").await;
        assert_eq!(hits, vec!["100% done"]);
    }

    #[tokio::test]
    async fn underscore_in_term_is_literal() {
        let hits = search(&["axb", "a_b"], "a_b").await;
        assert_eq!(hits, vec!["a_b"]);
    }

    #[tokio::test]
    async fn backslash_and_glob_metacharacters_are_literal() {
        let hits = search(&["abc", "a*c", "a?c", "a[b]c", "a\\c"], "a*c").await;
        assert_eq!(hits, vec!["a*c"]);
        let hits = search(&["abc", "a*c", "a?c", "a[b]c", "a\\c"], "a?c").await;
        assert_eq!(hits, vec!["a?c"]);
        let hits = search(&["abc", "a*c", "a?c", "a[b]c", "a\\c"], "a[b]c").await;
        assert_eq!(hits, vec!["a[b]c"]);
        let hits = search(&["abc", "a*c", "a?c", "a[b]c", "a\\c"], "a\\c").await;
        assert_eq!(hits, vec!["a\\c"]);
    }

    #[tokio::test]
    async fn matching_is_unicode_case_insensitive() {
        // SQLite's LOWER()/LIKE fold only ASCII; the archive must agree
        // with the in-memory `matches_fulltext` on non-ASCII text.
        assert_eq!(search(&["\u{dc}ber alles"], "\u{fc}ber").await.len(), 1);
        assert_eq!(search(&["\u{fc}ber alles"], "\u{dc}BER").await.len(), 1);
        assert_eq!(search(&["Hello World"], "wORLD").await.len(), 1);
        assert_eq!(
            search(
                &["\u{3a3}\u{39f}\u{3a6}\u{39f}\u{3a3}"],
                "\u{3c3}\u{3bf}\u{3c6}\u{3bf}"
            )
            .await
            .len(),
            1
        );
        assert!(search(&["Uber alles"], "\u{fc}ber").await.is_empty());
    }

    #[tokio::test]
    async fn all_terms_must_match() {
        let hits = search(&["release notes", "release"], "release NOTES").await;
        assert_eq!(hits, vec!["release notes"]);
    }
}

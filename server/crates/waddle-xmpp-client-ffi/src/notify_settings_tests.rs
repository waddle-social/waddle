//! XEP-0492 FFI test suite.
//!
//! Mirrors the wasm crate's XEP-0492 boundary cases — item-not-found
//! → empty list, rich-payload opt-in read, fallback-mode read across
//! notify siblings, typed outcome mapping — against the exact FFI
//! construction/mapping paths, plus the exported verbs' typed
//! not-connected / invalid-JID failure behavior.

use std::sync::{Arc, Mutex as StdMutex};

use jid::BareJid;
use minidom::Element;

use waddle_xmpp_client::error::{
    ClientError, StanzaError, StanzaErrorType, STANZA_CONDITION_ITEM_NOT_FOUND,
};
use waddle_xmpp_client::notification_settings::{
    SetDmNotificationModeOutcome, SetRoomNotificationModeOutcome,
};
use waddle_xmpp_client::xep::xep0402::BookmarkItem;
use waddle_xmpp_client::xep::xep0492::NotifyMode;

use crate::notify_settings::{
    bookmark_item_to_ffi, bookmarks_from_fetch_result, dm_bookmarks_from_fetch_result,
    dm_outcome_to_ffi, normalize_bookmark_name, parse_bookmark_target_jid,
    parse_dm_bookmarks_response, room_outcome_to_ffi,
};
use crate::{
    WaddleClient, WaddleClientEvent, WaddleConfig, WaddleDmBookmarkItem, WaddleError,
    WaddleEventListener, WaddleNotifyMode, WaddleSetDmNotificationModeOutcome,
    WaddleSetRoomNotificationModeOutcome,
};

#[derive(Clone, Default)]
struct RecordingListener {
    errors: Arc<StdMutex<Vec<String>>>,
}

impl WaddleEventListener for RecordingListener {
    fn on_event(&self, event: WaddleClientEvent) {
        if let WaddleClientEvent::Error { description } = event {
            self.errors
                .lock()
                .expect("test mutex poisoned")
                .push(description);
        }
    }
}

fn test_client() -> Arc<WaddleClient> {
    Arc::new(WaddleClient {
        config: WaddleConfig {
            server_url: "wss://xmpp.waddle.test".to_string(),
            jid: "alice@waddle.test".to_string(),
            access_token: "token".to_string(),
            resource: "test".to_string(),
            resume_state: None,
        },
        listener: Arc::new(Box::new(RecordingListener::default()) as Box<dyn WaddleEventListener>),
        handle: tokio::sync::Mutex::new(None),
        inbox_query_gate: tokio::sync::Mutex::new(()),
    })
}

fn stanza_error(condition: &str) -> ClientError {
    ClientError::StanzaError(StanzaError {
        error_type: StanzaErrorType::Cancel,
        condition: condition.to_string(),
        text: None,
        application_condition: None,
    })
}

fn bookmark_with_extensions(extensions_xml: &str) -> BookmarkItem {
    let extensions: Element = extensions_xml.parse().expect("valid extensions");
    BookmarkItem {
        jid: "room@muc.waddle.test".parse().expect("valid jid"),
        name: Some("Room".to_string()),
        autojoin: true,
        nick: None,
        password: None,
        extensions: extensions.children().cloned().collect(),
    }
}

// ── Fetch: item-not-found → empty list (wasm parity) ────────────────────────

#[test]
fn bookmarks_fetch_item_not_found_yields_empty_list() {
    let items = bookmarks_from_fetch_result(Err(stanza_error(STANZA_CONDITION_ITEM_NOT_FOUND)))
        .expect("empty list, not an error");
    assert!(items.is_empty());
}

#[test]
fn dm_bookmarks_fetch_item_not_found_yields_empty_list() {
    let items = dm_bookmarks_from_fetch_result(Err(stanza_error(STANZA_CONDITION_ITEM_NOT_FOUND)))
        .expect("empty list, not an error");
    assert!(items.is_empty());
}

#[test]
fn bookmarks_fetch_other_stanza_error_is_typed() {
    let err = bookmarks_from_fetch_result(Err(stanza_error("forbidden")))
        .expect_err("forbidden must surface");
    assert_eq!(
        err,
        WaddleError::Stanza {
            condition: "forbidden".to_string(),
            text: None,
        }
    );
}

#[test]
fn dm_bookmarks_fetch_other_stanza_error_is_typed() {
    let err = dm_bookmarks_from_fetch_result(Err(stanza_error("forbidden")))
        .expect_err("forbidden must surface");
    assert_eq!(
        err,
        WaddleError::Stanza {
            condition: "forbidden".to_string(),
            text: None,
        }
    );
}

#[test]
fn bookmarks_fetch_result_parses_items() {
    let response: Element = "<iq xmlns='jabber:client' type='result'>\
            <pubsub xmlns='http://jabber.org/protocol/pubsub'>\
                <items node='urn:xmpp:bookmarks:1'>\
                    <item id='room@muc.waddle.test'>\
                        <conference xmlns='urn:xmpp:bookmarks:1' name='Room' autojoin='true'>\
                            <extensions>\
                                <notify xmlns='urn:xmpp:notification-settings:1'><never/></notify>\
                            </extensions>\
                        </conference>\
                    </item>\
                </items>\
            </pubsub>\
        </iq>"
        .parse()
        .expect("valid iq");
    let items = bookmarks_from_fetch_result(Ok(response)).expect("items parse");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].jid, "room@muc.waddle.test");
    assert_eq!(items[0].name.as_deref(), Some("Room"));
    assert!(items[0].autojoin);
    assert_eq!(items[0].notify_mode, Some(WaddleNotifyMode::Never));
    assert!(!items[0].rich_payload_opt_in);
}

// ── Surfacing: fallback-mode + rich-payload opt-in reads (wasm parity) ──────

#[test]
fn surfaced_bookmark_reads_rich_payload_opt_in() {
    let item = bookmark_with_extensions(
        "<extensions xmlns='urn:xmpp:bookmarks:1'>\
            <notify xmlns='urn:xmpp:notification-settings:1'>\
                <always>\
                    <advanced xmlns='urn:xmpp:notification-settings:1'>\
                        <rich-payload xmlns='urn:waddle:push:rich:0'/>\
                    </advanced>\
                </always>\
            </notify>\
        </extensions>",
    );
    let surfaced = bookmark_item_to_ffi(item);
    assert_eq!(surfaced.notify_mode, Some(WaddleNotifyMode::Always));
    assert!(surfaced.rich_payload_opt_in);
}

#[test]
fn surfaced_bookmark_opt_in_defaults_off_without_advanced() {
    let item = bookmark_with_extensions(
        "<extensions xmlns='urn:xmpp:bookmarks:1'>\
            <notify xmlns='urn:xmpp:notification-settings:1'><never /></notify>\
        </extensions>",
    );
    let surfaced = bookmark_item_to_ffi(item);
    assert_eq!(surfaced.notify_mode, Some(WaddleNotifyMode::Never));
    assert!(!surfaced.rich_payload_opt_in);
}

#[test]
fn surfaced_bookmark_without_notify_resolves_to_none() {
    // No <notify/> extension — the Kotlin caller resolves against the
    // XEP-0492 §3 conversation-kind default.
    let item = bookmark_with_extensions("<extensions xmlns='urn:xmpp:bookmarks:1'/>");
    let surfaced = bookmark_item_to_ffi(item);
    assert_eq!(surfaced.notify_mode, None);
    assert!(!surfaced.rich_payload_opt_in);
}

#[test]
fn surfaced_bookmark_scans_past_fallbackless_notify_sibling() {
    // Malformed-but-possible state: the first <notify/> holds only an
    // identity-scoped setting; the real fallback + opt-in live in the
    // second. ALL siblings must be scanned (wasm parity).
    let item = bookmark_with_extensions(
        "<extensions xmlns='urn:xmpp:bookmarks:1'>\
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
        </extensions>",
    );
    let surfaced = bookmark_item_to_ffi(item);
    assert_eq!(surfaced.notify_mode, Some(WaddleNotifyMode::Always));
    assert!(surfaced.rich_payload_opt_in);
}

// ── DM items response parsing (wasm parity) ─────────────────────────────────

#[test]
fn parse_dm_bookmarks_response_reads_notify_and_skips_domain_only_id() {
    // A domain-only id (no localpart) is not a valid DM contact JID —
    // it must be skipped so the UI never surfaces an impossible DM
    // entry. A well-formed sibling still parses.
    let iq: Element = "<iq xmlns='jabber:client'>\
            <pubsub xmlns='http://jabber.org/protocol/pubsub'>\
                <items node='urn:waddle:dm-bookmarks:0'>\
                    <item id='bob@waddle.test'>\
                        <dm-bookmark xmlns='urn:waddle:dm-bookmarks:0'>\
                            <notify xmlns='urn:xmpp:notification-settings:1'><never/></notify>\
                        </dm-bookmark>\
                    </item>\
                    <item id='waddle.test'>\
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
    assert_eq!(
        parsed[0],
        WaddleDmBookmarkItem {
            jid: "bob@waddle.test".to_string(),
            notify_mode: Some(WaddleNotifyMode::Never),
            rich_payload_opt_in: false,
        }
    );
}

#[test]
fn parse_dm_bookmarks_response_missing_notify_yields_no_fallback() {
    // A malformed payload without its hosted <notify/> surfaces with
    // notify_mode = None — the caller resolves the §3 direct-chat
    // default (`always`).
    let iq: Element = "<iq xmlns='jabber:client'>\
            <pubsub xmlns='http://jabber.org/protocol/pubsub'>\
                <items node='urn:waddle:dm-bookmarks:0'>\
                    <item id='bob@waddle.test'>\
                        <dm-bookmark xmlns='urn:waddle:dm-bookmarks:0'/>\
                    </item>\
                </items>\
            </pubsub>\
        </iq>"
        .parse()
        .expect("valid iq");
    let parsed = parse_dm_bookmarks_response(&iq);
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].notify_mode, None);
    assert!(!parsed[0].rich_payload_opt_in);
}

// ── Outcome mapping ──────────────────────────────────────────────────────────

#[test]
fn room_outcome_ok_surfaces_merged_item() {
    let item = bookmark_with_extensions(
        "<extensions xmlns='urn:xmpp:bookmarks:1'>\
            <notify xmlns='urn:xmpp:notification-settings:1'><on-mention/></notify>\
        </extensions>",
    );
    let outcome = room_outcome_to_ffi(SetRoomNotificationModeOutcome::Ok { item });
    let WaddleSetRoomNotificationModeOutcome::Ok { item } = outcome else {
        panic!("expected ok outcome");
    };
    assert_eq!(item.jid, "room@muc.waddle.test");
    assert_eq!(item.name.as_deref(), Some("Room"));
    assert!(item.autojoin);
    assert_eq!(item.notify_mode, Some(WaddleNotifyMode::OnMention));
    assert!(!item.rich_payload_opt_in);
}

#[test]
fn room_outcome_maps_mismatch_and_error() {
    assert_eq!(
        room_outcome_to_ffi(SetRoomNotificationModeOutcome::NodeConfigMismatch),
        WaddleSetRoomNotificationModeOutcome::NodeConfigMismatch,
    );
    assert_eq!(
        room_outcome_to_ffi(SetRoomNotificationModeOutcome::Error),
        WaddleSetRoomNotificationModeOutcome::Error,
    );
}

#[test]
fn dm_outcome_ok_carries_typed_notify_values() {
    let jid: BareJid = "bob@waddle.test".parse().expect("valid jid");
    let outcome = dm_outcome_to_ffi(SetDmNotificationModeOutcome::Ok {
        jid,
        mode: NotifyMode::Never,
        rich_payload_opt_in: true,
    });
    assert_eq!(
        outcome,
        WaddleSetDmNotificationModeOutcome::Ok {
            item: WaddleDmBookmarkItem {
                jid: "bob@waddle.test".to_string(),
                notify_mode: Some(WaddleNotifyMode::Never),
                rich_payload_opt_in: true,
            },
        }
    );
}

#[test]
fn dm_outcome_maps_removed_mismatch_and_error() {
    let jid: BareJid = "bob@waddle.test".parse().expect("valid jid");
    assert_eq!(
        dm_outcome_to_ffi(SetDmNotificationModeOutcome::Removed { jid }),
        WaddleSetDmNotificationModeOutcome::Removed {
            jid: "bob@waddle.test".to_string(),
        },
    );
    assert_eq!(
        dm_outcome_to_ffi(SetDmNotificationModeOutcome::NodeConfigMismatch),
        WaddleSetDmNotificationModeOutcome::NodeConfigMismatch,
    );
    assert_eq!(
        dm_outcome_to_ffi(SetDmNotificationModeOutcome::Error),
        WaddleSetDmNotificationModeOutcome::Error,
    );
}

// ── Input validation ─────────────────────────────────────────────────────────

#[test]
fn bookmark_target_jid_requires_localpart() {
    // XEP-0402 §2.2 bookmark ids / DM-bookmark item ids MUST have a
    // localpart; a domain-only JID is rejected early as a typed error.
    assert_eq!(
        parse_bookmark_target_jid("muc.waddle.test"),
        Err(WaddleError::InvalidJid),
    );
    assert_eq!(
        parse_bookmark_target_jid("not a jid"),
        Err(WaddleError::InvalidJid),
    );
    assert_eq!(
        parse_bookmark_target_jid("room@muc.waddle.test"),
        Ok("room@muc.waddle.test".parse().expect("valid jid")),
    );
}

#[test]
fn bookmark_name_normalizes_empty_and_whitespace_to_none() {
    // An empty/whitespace-only name would publish `<conference
    // name=""/>`, which the parser round-trips as `None` — normalize
    // for a consistent wire shape (wasm parity).
    assert_eq!(normalize_bookmark_name(None), None);
    assert_eq!(normalize_bookmark_name(Some(String::new())), None);
    assert_eq!(normalize_bookmark_name(Some("   ".to_string())), None);
    assert_eq!(
        normalize_bookmark_name(Some("  Room  ".to_string())),
        Some("Room".to_string()),
    );
}

#[test]
fn iq_timeout_collapses_to_typed_timeout_error() {
    // `TimeoutCommandDriver` bounds every set-verb IQ with the crate
    // IQ timeout; the resulting `IqTimeout` must surface as the typed
    // `Timeout` (via `client_error_to_waddle` on the verbs' Err path),
    // never fold into the stanza-level `Error` outcome.
    assert_eq!(
        crate::error::client_error_to_waddle(&ClientError::IqTimeout {
            timeout: std::time::Duration::from_secs(30),
        }),
        WaddleError::Timeout,
    );
}

// ── Exported verbs: typed not-connected / invalid-JID failures ──────────────

#[tokio::test]
async fn fetch_user_bookmarks_requires_connection() {
    let client = test_client();
    assert_eq!(
        client.fetch_user_bookmarks().await,
        Err(WaddleError::NotConnected),
    );
}

#[tokio::test]
async fn fetch_dm_bookmarks_requires_connection() {
    let client = test_client();
    assert_eq!(
        client.fetch_dm_bookmarks().await,
        Err(WaddleError::NotConnected),
    );
}

#[tokio::test]
async fn set_room_notification_mode_rejects_domain_only_jid() {
    let client = test_client();
    assert_eq!(
        client
            .set_room_notification_mode(
                "muc.waddle.test".to_string(),
                WaddleNotifyMode::Never,
                None,
                false,
            )
            .await,
        Err(WaddleError::InvalidJid),
    );
}

#[tokio::test]
async fn set_room_notification_mode_requires_connection() {
    let client = test_client();
    assert_eq!(
        client
            .set_room_notification_mode(
                "room@muc.waddle.test".to_string(),
                WaddleNotifyMode::OnMention,
                Some("Room".to_string()),
                false,
            )
            .await,
        Err(WaddleError::NotConnected),
    );
}

#[tokio::test]
async fn set_dm_notification_mode_rejects_domain_only_jid() {
    let client = test_client();
    assert_eq!(
        client
            .set_dm_notification_mode("waddle.test".to_string(), WaddleNotifyMode::Never, false)
            .await,
        Err(WaddleError::InvalidJid),
    );
}

#[tokio::test]
async fn set_dm_notification_mode_requires_connection() {
    let client = test_client();
    assert_eq!(
        client
            .set_dm_notification_mode(
                "bob@waddle.test".to_string(),
                WaddleNotifyMode::Never,
                false,
            )
            .await,
        Err(WaddleError::NotConnected),
    );
}

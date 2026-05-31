//! Per-DM XEP-0492 notification-settings end-to-end slice (#720).
//!
//! Per-conversation notification levels for **direct (one-to-one) chats**
//! are carried over the Waddle-custom PEP node `urn:waddle:dm-bookmarks:0`
//! (the DM counterpart to the XEP-0402 MUC carrier). The decision is
//! recorded in ADR-009 (`docs/adr/009-dm-notification-carrier.md`) and the
//! normative wire reference is `docs/specs/urn-waddle-dm-bookmarks.md`. The
//! payload shape is:
//!
//! ```xml
//! <item id='bob@example.com'>
//!   <dm-bookmark xmlns='urn:waddle:dm-bookmarks:0'>
//!     <notify xmlns='urn:xmpp:notification-settings:1'><never/></notify>
//!   </dm-bookmark>
//! </item>
//! ```
//!
//! The PEP item id is the contact's bare JID; the node is sparse and
//! override-only — "an item exists" means "this DM has an override", and
//! returning to the §3 `<always/>` default is expressed by *retracting*
//! the item (XEP-0492 §3; the dm-bookmarks spec "Item lifecycle" section).
//!
//! ## E2E level implemented: storage-entry-point (NOT WS round-trip)
//!
//! This suite drives the DM slice one layer below the WebSocket transport:
//! it exercises the real server pubsub entry points
//! ([`DatabasePubSubStorage::publish_item`] / [`retract_item`] — the exact
//! methods the WS stanza handler invokes after parsing an `<iq type='set'>`
//! pubsub publish/retract) against a real on-disk-shaped `sqlite::memory:`
//! backend, then reads the resulting [`NotificationSettingsProjectionStore`]
//! and feeds the resolved level through the production push gate
//! [`PushDispatchDecision::evaluate`]. The shared `ws_common` WS harness
//! (`tests/ws_common/mod.rs`) spawns the server as a *separate process* and
//! exposes only the wire: it cannot observe the in-process projection store
//! or the push-dispatch decision (the push fan-out targets a real
//! provider, not the WS client), so a true WS round-trip cannot assert the
//! server-side projection or the push gate without standing up a mock push
//! provider — large new scaffolding outside this issue's scope. The
//! storage-entry-point level is therefore the *strongest reusable* e2e
//! level: it ties the DM carrier projection to the push gate end-to-end,
//! which neither the store-only DM tests in `src/pubsub/tests.rs` (they
//! stop at the projection row) nor the pure-reducer matrix in
//! `tests/xep0492_push_enforcement_ws.rs` (it never touches storage)
//! connects.

use waddle_server::notification_settings_projection::{
    ConversationKind, NotificationSettingsProjectionStore, NotificationSettingsSource,
    PushDispatchDecision,
};
use waddle_server::pubsub::DatabasePubSubStorage;
use waddle_xmpp::pubsub::{PubSubItem, PubSubStorage};
use waddle_xmpp::xep::xep_waddle_dm_bookmarks::PEP_NODE_WADDLE_DM_BOOKMARKS;
use waddle_xmpp::xep::NotificationLevel;
use xmpp_parsers::minidom::Element;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn bare_jid(value: &str) -> jid::BareJid {
    value.parse().expect("valid bare JID")
}

/// Build a `<dm-bookmark>` carrier payload hosting the given `<notify>`
/// inner XML. Test fixture XML is assembled via `.parse()` into a
/// `minidom::Element`, matching the established style in
/// `src/pubsub/tests.rs::dm_bookmark_payload`. The production path builds
/// these with typed builders; tests parse fixtures for readability.
fn dm_bookmark_payload(notify_inner: &str) -> Element {
    format!("<dm-bookmark xmlns='urn:waddle:dm-bookmarks:0'>{notify_inner}</dm-bookmark>")
        .parse()
        .expect("valid dm-bookmark payload")
}

/// A `<dm-bookmark>` item keyed on the contact bare JID (the PEP item id),
/// per the wire spec: `id` == contact bare JID.
fn dm_bookmark_item(contact: &jid::BareJid, notify_inner: &str) -> PubSubItem {
    PubSubItem {
        id: Some(contact.to_string()),
        publisher: None,
        payload: Some(dm_bookmark_payload(notify_inner)),
    }
}

const NOTIFY_NEVER: &str = "<notify xmlns='urn:xmpp:notification-settings:1'><never /></notify>";
const NOTIFY_ON_MENTION: &str =
    "<notify xmlns='urn:xmpp:notification-settings:1'><on-mention /></notify>";

/// Open an in-memory pubsub backend, instantiate the projection store over
/// the SAME database handle (so projection mutations derived inside the
/// publish tx are visible), and seed the owner's DM-bookmarks node.
async fn setup_owner_node(
    owner: &jid::BareJid,
) -> (DatabasePubSubStorage, NotificationSettingsProjectionStore) {
    let storage = DatabasePubSubStorage::open(Some("sqlite::memory:"))
        .await
        .expect("open pubsub storage");
    let projection = NotificationSettingsProjectionStore::new(storage.database());
    storage
        .get_or_create_node(owner, PEP_NODE_WADDLE_DM_BOOKMARKS)
        .await
        .expect("create dm-bookmarks node");
    (storage, projection)
}

// ---------------------------------------------------------------------------
// `<never/>` — DM mute. Publish writes a Direct/Never row; the push gate
// then suppresses with the typed `Never` reason. Retract returns to the
// §3 default and the gate flips back to Deliver.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dm_never_override_projects_direct_row_and_suppresses_push() {
    let owner = bare_jid("alice@example.com");
    let contact = bare_jid("bob@example.com");
    let (storage, projection) = setup_owner_node(&owner).await;

    // WS-equivalent publish entry point: <iq type='set'> pubsub publish of
    // a <dm-bookmark><notify><never/></notify></dm-bookmark> item.
    storage
        .publish_item(
            &owner,
            PEP_NODE_WADDLE_DM_BOOKMARKS,
            &dm_bookmark_item(&contact, NOTIFY_NEVER),
            Some(&owner),
            false,
        )
        .await
        .expect("publish DM <never/> override");

    // The carrier published a Direct projection row keyed on the contact.
    let row = projection
        .get(&owner, &contact)
        .await
        .expect("read projection")
        .expect("Direct row present after publish");
    assert_eq!(row.conversation_kind, ConversationKind::Direct);
    assert_eq!(row.mode, NotificationLevel::Never);
    assert_eq!(row.source, NotificationSettingsSource::WaddleDmBookmarks);
    assert_eq!(row.source_item_jid, contact);

    // End-to-end through the push gate: a muted DM yields Suppressed(Never)
    // regardless of mention state.
    let level = projection
        .effective_setting(&owner, &contact, ConversationKind::Direct)
        .await
        .expect("resolve effective level");
    assert_eq!(
        PushDispatchDecision::evaluate(level, false),
        PushDispatchDecision::Suppressed {
            reason: NotificationLevel::Never
        },
        "muted DM (<never/>) must suppress push for an ordinary message"
    );
    assert_eq!(
        PushDispatchDecision::evaluate(level, true),
        PushDispatchDecision::Suppressed {
            reason: NotificationLevel::Never
        },
        "muted DM (<never/>) must suppress push even when the message mentions the recipient"
    );

    // Return-to-default: retracting the item clears the row, and the gate
    // falls back to the §3 Direct default (<always/>) → Deliver.
    let retracted = storage
        .retract_item(&owner, PEP_NODE_WADDLE_DM_BOOKMARKS, &contact.to_string())
        .await
        .expect("retract DM override");
    assert!(retracted, "the published DM item must be retracted");
    assert!(
        projection
            .get(&owner, &contact)
            .await
            .expect("read projection after retract")
            .is_none(),
        "retract must clear the Direct projection row (absence == §3 default)"
    );

    let default_level = projection
        .effective_setting(&owner, &contact, ConversationKind::Direct)
        .await
        .expect("resolve default level after retract");
    assert_eq!(
        default_level,
        NotificationLevel::Always,
        "absence of a DM override resolves to the §3 Direct default <always/>"
    );
    assert_eq!(
        PushDispatchDecision::evaluate(default_level, false),
        PushDispatchDecision::Deliver,
        "a defaulted DM must deliver push after the override is retracted"
    );
}

// ---------------------------------------------------------------------------
// `<on-mention/>` — DM mentions-only. Publish writes a Direct/OnMention
// row; the push gate delivers only when the message mentions the
// recipient, otherwise suppresses with the typed `OnMention` reason.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dm_on_mention_override_gates_push_on_mention_bit() {
    let owner = bare_jid("alice@example.com");
    let contact = bare_jid("carol@example.com");
    let (storage, projection) = setup_owner_node(&owner).await;

    storage
        .publish_item(
            &owner,
            PEP_NODE_WADDLE_DM_BOOKMARKS,
            &dm_bookmark_item(&contact, NOTIFY_ON_MENTION),
            Some(&owner),
            false,
        )
        .await
        .expect("publish DM <on-mention/> override");

    let row = projection
        .get(&owner, &contact)
        .await
        .expect("read projection")
        .expect("Direct row present after publish");
    assert_eq!(row.conversation_kind, ConversationKind::Direct);
    assert_eq!(row.mode, NotificationLevel::OnMention);
    assert_eq!(row.source, NotificationSettingsSource::WaddleDmBookmarks);

    let level = projection
        .effective_setting(&owner, &contact, ConversationKind::Direct)
        .await
        .expect("resolve effective level");
    assert_eq!(
        PushDispatchDecision::evaluate(level, false),
        PushDispatchDecision::Suppressed {
            reason: NotificationLevel::OnMention
        },
        "an <on-mention/> DM must suppress push for a non-mention message"
    );
    assert_eq!(
        PushDispatchDecision::evaluate(level, true),
        PushDispatchDecision::Deliver,
        "an <on-mention/> DM must deliver push when the message mentions the recipient"
    );
}

// ---------------------------------------------------------------------------
// Default (no override) — the sparse node holds no item, so the gate
// resolves the §3 Direct default <always/> → Deliver. This pins the
// "absence == always" contract end-to-end through the push gate without a
// publish ever happening.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dm_without_override_delivers_push_via_section3_default() {
    let owner = bare_jid("alice@example.com");
    let contact = bare_jid("dave@example.com");
    let (_storage, projection) = setup_owner_node(&owner).await;

    assert!(
        projection
            .get(&owner, &contact)
            .await
            .expect("read projection")
            .is_none(),
        "no DM override published → no projection row"
    );

    let level = projection
        .effective_setting(&owner, &contact, ConversationKind::Direct)
        .await
        .expect("resolve effective level");
    assert_eq!(
        level,
        NotificationLevel::Always,
        "a DM with no override resolves to the §3 Direct default <always/>"
    );
    assert_eq!(
        PushDispatchDecision::evaluate(level, false),
        PushDispatchDecision::Deliver,
        "a defaulted DM delivers push for an ordinary message"
    );
    assert_eq!(
        PushDispatchDecision::evaluate(level, true),
        PushDispatchDecision::Deliver,
        "a defaulted DM delivers push for a mention too"
    );
}

// ---------------------------------------------------------------------------
// Malformed DM `<notify>` — a carrier hosting two account-wide fallback
// children is invalid per XEP-0492 §2.1. The publish entry point MUST
// reject it with a <bad-request/> stanza error and leave the node empty,
// so no spurious projection row is derived.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dm_malformed_notify_is_bad_request_and_writes_no_row() {
    let owner = bare_jid("alice@example.com");
    let contact = bare_jid("erin@example.com");
    let (storage, projection) = setup_owner_node(&owner).await;

    let malformed = dm_bookmark_item(
        &contact,
        "<notify xmlns='urn:xmpp:notification-settings:1'><always /><never /></notify>",
    );
    let err = storage
        .publish_item(
            &owner,
            PEP_NODE_WADDLE_DM_BOOKMARKS,
            &malformed,
            Some(&owner),
            false,
        )
        .await
        .expect_err("malformed DM <notify> must be rejected");
    match &err {
        waddle_xmpp::XmppError::Stanza { condition, .. } => assert!(
            format!("{condition:?}")
                .to_lowercase()
                .contains("badrequest"),
            "expected <bad-request/> condition, got: {condition:?}"
        ),
        other => panic!("expected Stanza bad-request error, got: {other:?}"),
    }

    assert!(
        projection
            .get(&owner, &contact)
            .await
            .expect("read projection after rejected publish")
            .is_none(),
        "a rejected malformed publish must not derive a projection row"
    );
    let stored = storage
        .get_items(&owner, PEP_NODE_WADDLE_DM_BOOKMARKS, None, &[])
        .await
        .expect("read items after rejected publish");
    assert!(
        stored.is_empty(),
        "a rejected malformed publish must not leave a pubsub_items row"
    );
}

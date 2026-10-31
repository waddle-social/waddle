use serde_json::{json, Value};

use crate::{TimelineCommand, TimelineSnapshot, TimelineStore};

const ROOM: &str = "general@rooms.example.org";
const ACCOUNT: &str = "david@example.org";

fn apply(store: &mut TimelineStore, method: &str, args: Value) -> TimelineSnapshot {
    let command: TimelineCommand = serde_json::from_value(json!({"method":method,"args":args}))
        .expect("valid typed boundary request");
    store.execute(command).expect("valid store operation")
}

fn ingest(store: &mut TimelineStore, messages: Value, archived: bool) -> TimelineSnapshot {
    apply(
        store,
        "timeline_ingest",
        json!({
            "conversation_jid":ROOM, "account_jid":ACCOUNT,"group":true,"own_nick":"david",
            "messages":messages,"archived":archived,"complete":archived,"received_at_ms":9000,
        }),
    )
}

fn room_message(author: &str, id: &str, body: &str) -> Value {
    json!({
        "id":id,"origin_id":id,"from":format!("{ROOM}/{author}"),
        "message_type":"groupchat","body":body,"timestamp":"2026-10-31T10:00:00Z",
        "stanza_id":"untrusted-convenience-id","stanza_id_by":"mallory@example.org",
        "stanza_ids":[{"id":format!("archive-{id}"),"by":ROOM}],
    })
}

#[test]
fn spoofed_stanza_authority_cannot_become_a_room_action_target() {
    let mut store = TimelineStore::new();
    let mut message = room_message("alex", "origin", "hello");
    message["stanza_ids"] = json!([{"id":"spoofed","by":"mallory@example.org"}]);
    let snapshot = ingest(&mut store, json!([message]), false);
    assert_eq!(snapshot.items.len(), 1);
    assert_eq!(snapshot.items[0].id, "origin");
    assert_eq!(snapshot.items[0].reference_id, None);
    assert_eq!(snapshot.items[0].archive_id, None);
    assert_eq!(snapshot.items[0].correction_id.as_deref(), Some("origin"));
}

#[test]
fn corrections_are_author_checked_buffered_and_latest_wins() {
    let mut store = TimelineStore::new();
    let mut attacker = room_message("mallory", "bad-edit", "forged edit");
    attacker["replaces_id"] = json!("original");
    attacker["timestamp"] = json!("2026-10-31T10:04:00Z");
    let mut newer = room_message("alex", "new-edit", "latest correction");
    newer["replaces_id"] = json!("original");
    newer["timestamp"] = json!("2026-10-31T10:03:00Z");
    let mut older = room_message("alex", "old-edit", "old correction");
    older["replaces_id"] = json!("original");
    older["timestamp"] = json!("2026-10-31T10:02:00Z");
    let _ = ingest(&mut store, json!([attacker, newer, older]), false);
    let snapshot = ingest(
        &mut store,
        json!([room_message("alex", "original", "original body")]),
        false,
    );
    assert_eq!(snapshot.items.len(), 1);
    assert_eq!(snapshot.items[0].body, "latest correction");
    assert!(snapshot.items[0].edited);
}

#[test]
fn own_echo_history_and_late_failure_dedupe_with_acceptance_precedence() {
    let mut store = TimelineStore::new();
    let _ = apply(
        &mut store,
        "timeline_local_echo",
        json!({
            "conversation_jid":ROOM,"account_jid":ACCOUNT,"group":true,"own_nick":"david",
            "origin_id":"origin","body":"Hello","timestamp_ms":1,
        }),
    );
    let live = room_message("david", "origin", "Hello");
    let _ = ingest(&mut store, json!([live.clone()]), false);
    let mut archived = live;
    archived["mam_id"] = json!("archive-origin");
    archived["author_real_jid"] = json!(ACCOUNT);
    let _ = ingest(&mut store, json!([archived]), true);
    let snapshot = apply(
        &mut store,
        "timeline_delivery",
        json!({
            "conversation_jid":ROOM,"origin_id":"origin","delivery":"failed"
        }),
    );
    assert_eq!(snapshot.items.len(), 1);
    assert!(snapshot.items[0].own);
    assert_eq!(snapshot.items[0].delivery, "sent");
    assert_eq!(
        snapshot.items[0]
            .archive_by
            .as_ref()
            .map(ToString::to_string)
            .as_deref(),
        Some(ROOM)
    );
}

#[test]
fn moderation_only_from_bare_room_and_reaction_sets_replace() {
    let mut store = TimelineStore::new();
    let original = room_message("alex", "original", "hello");
    let mut reaction = room_message("sam", "reaction", "");
    reaction["body"] = Value::Null;
    reaction["reaction_target_id"] = json!("archive-original");
    reaction["reaction_emojis"] = json!(["👍", "👍"]);
    let mut forged = room_message("mallory", "moderate", "");
    forged["body"] = Value::Null;
    forged["moderation_target_id"] = json!("archive-original");
    let snapshot = ingest(&mut store, json!([original, reaction, forged]), false);
    assert!(!snapshot.items[0].retracted);
    assert_eq!(snapshot.items[0].reactions[0].count, 1);
    let snapshot = ingest(
        &mut store,
        json!([{
            "from":ROOM,"message_type":"groupchat","moderation_target_id":"archive-original"
        }]),
        false,
    );
    assert!(snapshot.items[0].retracted);
}

#[test]
fn direct_messages_reject_foreign_conversations_and_only_trust_account_archive() {
    let mut store = TimelineStore::new();
    let snapshot = apply(
        &mut store,
        "timeline_ingest",
        json!({
            "conversation_jid":"alex@example.org","account_jid":ACCOUNT,"group":false,
            "messages":[{
                "id":"wrong","from":"mallory@example.org/phone","message_type":"chat","body":"foreign"
            },{
                "id":"sender-id","from":"alex@example.org/phone","message_type":"chat","body":"hello",
                "displayed_marker_requested":true,
                "stanza_ids":[{"id":"spoofed","by":"mallory@example.org"},{"id":"verified","by":"example.org"}]
            }]
        }),
    );
    assert_eq!(snapshot.items.len(), 1);
    assert_eq!(snapshot.items[0].id, "verified");
    assert_eq!(snapshot.items[0].reference_id.as_deref(), Some("sender-id"));
    assert_eq!(
        snapshot.items[0]
            .archive_by
            .as_ref()
            .map(ToString::to_string)
            .as_deref(),
        Some("example.org")
    );
    assert!(snapshot.items[0].marker_requested);
}

#[test]
fn account_changes_require_an_explicit_reset() {
    let mut store = TimelineStore::new();
    let _ = ingest(&mut store, json!([]), false);
    let command = serde_json::from_value(json!({"method":"timeline_ingest","args":{
        "conversation_jid":ROOM,"account_jid":"other@example.org","group":true,"messages":[]
    }}))
    .expect("request");
    assert!(store.execute(command).is_err());
    let _ = apply(&mut store, "timeline_reset", json!({}));
    let snapshot = apply(
        &mut store,
        "timeline_snapshot",
        json!({"conversation_jid":ROOM}),
    );
    assert!(snapshot.items.is_empty());
}

#[test]
fn call_anchors_without_body_and_messages_without_origin_id_remain_visible() {
    let mut store = TimelineStore::new();
    let snapshot = ingest(
        &mut store,
        json!([{
            "id":"call", "from":"general@rooms.example.org/alex", "message_type":"groupchat",
            "stanza_ids":[{"id":"room-call","by":ROOM}],
            "call_thread":{"kind":"muc","sid":"sid","media":["audio"],"initiator":"alex@example.org","started":"2026-10-31T10:00:00Z"}
        },{
            "id":"legacy","from":"general@rooms.example.org/alex","message_type":"groupchat","body":"Legacy text",
            "stanza_ids":[{"id":"untrusted-full","by":"general@rooms.example.org/attacker"}]
        }]),
        false,
    );
    assert_eq!(snapshot.items.len(), 2);
    assert_eq!(snapshot.items[0].id, "room-call");
    assert_eq!(snapshot.items[0].identity_ids, vec!["call", "room-call"]);
    assert_eq!(snapshot.items[1].id, "legacy");
    assert_eq!(snapshot.items[1].reference_id, None);
}

#[test]
fn judgment_authority_and_exact_revision_survive_both_binding_enum_shapes() {
    let mut store = TimelineStore::new();
    let original = room_message("alex", "original", "hello");
    let make_scores = |from: &str, by: &str, category: &str| {
        json!({
            "from":from,"message_type":"groupchat","safety_scores":{
                "target_origin_id":"original","target_stanza_id":"archive-original","target_stanza_by":by,
                "source_revision_id":"archive-original","scores":{"model_version":"v1","scores":[{
                    "category":category,"probability":0.9,"taxonomy_version":"v1"
                }]}
            }
        })
    };
    let _ = ingest(&mut store, json!([original]), false);
    let forged = ingest(
        &mut store,
        json!([make_scores(
            "general@rooms.example.org/mallory",
            ROOM,
            "hate_speech"
        )]),
        false,
    );
    assert!(forged.items[0].scores.is_none());
    let forged = ingest(
        &mut store,
        json!([make_scores(ROOM, "other@rooms.example.org", "HateSpeech")]),
        false,
    );
    assert!(forged.items[0].scores.is_none());
    let valid = ingest(
        &mut store,
        json!([make_scores(ROOM, ROOM, "hate_speech")]),
        false,
    );
    assert_eq!(
        valid.items[0].scores.as_ref().expect("scores").scores.len(),
        1
    );
}

#[test]
fn native_and_browser_carbon_dtos_produce_the_same_canonical_message() {
    let ingest_carbon = |carbon| {
        let mut store = TimelineStore::new();
        apply(
            &mut store,
            "timeline_ingest",
            json!({
                "conversation_jid":"alex@example.org","account_jid":ACCOUNT,"group":false,
                "messages":[{"id":"sent","from":ACCOUNT,"to":"alex@example.org/phone","message_type":"chat",
                    "body":"sent on another device","carbon":carbon}]
            }),
        )
    };
    let native = ingest_carbon(json!("sent"));
    let browser = ingest_carbon(json!({"sent":true,"received":false}));
    assert_eq!(
        serde_json::to_value(&native).expect("snapshot"),
        serde_json::to_value(&browser).expect("snapshot")
    );
    assert!(native.items[0].own);
    assert_eq!(native.items[0].author_id.as_deref(), Some(ACCOUNT));
    assert!(
        serde_json::from_value::<TimelineCommand>(json!({"method":"timeline_ingest","args":{
            "conversation_jid":"alex@example.org","account_jid":ACCOUNT,"group":false,
            "messages":[{"carbon":{"sent":true,"received":true}}]
        }}))
        .is_err()
    );
}

#[test]
fn native_and_browser_archived_call_only_dtos_remain_visible() {
    let ingest_call = |kind| {
        let mut store = TimelineStore::new();
        apply(
            &mut store,
            "timeline_ingest",
            json!({
                "conversation_jid":"alex@example.org","account_jid":ACCOUNT,"group":false,"archived":true,
                "messages":[{"mam_id":"call-archive","id":"call-propose","from":"alex@example.org/phone","to":ACCOUNT,"message_type":"chat",
                    "call_event":{"from":"alex@example.org/phone","to":ACCOUNT,"sid":"call-session","kind":kind}}]
            }),
        )
    };
    let native = ingest_call(json!({"type":"propose","media":{"audio":true,"video":false}}));
    let browser = ingest_call(json!("propose"));
    assert_eq!(native.items.len(), 1);
    assert_eq!(native.items[0].id, "call-archive");
    assert_eq!(native.items[0].body, "");
    assert_eq!(
        serde_json::to_value(native).expect("snapshot"),
        serde_json::to_value(browser).expect("snapshot")
    );
}

#[test]
fn live_presence_stamps_prevent_a_new_nickname_owner_editing_archive_history() {
    let mut store = TimelineStore::new();
    let mut archived = room_message("alex", "original", "Alex's text");
    archived["mam_id"] = json!("archive-original");
    archived["author_real_jid"] = json!("alex@example.org");
    let _ = ingest(&mut store, json!([archived]), true);
    let mut attacker = room_message("alex", "attack", "Mallory's edit");
    attacker["replaces_id"] = json!("original");
    attacker["_author_stamp"] = json!("mallory@example.org");
    attacker["timestamp"] = json!("2026-10-31T10:05:00Z");
    let snapshot = ingest(&mut store, json!([attacker]), false);
    assert_eq!(snapshot.items[0].body, "Alex's text");
    assert!(!snapshot.items[0].edited);
    let mut unstamped = room_message("alex", "unstamped", "Untrusted claim");
    unstamped["replaces_id"] = json!("original");
    unstamped["author_real_jid"] = json!("alex@example.org");
    let snapshot = ingest(&mut store, json!([unstamped]), false);
    assert_eq!(
        snapshot.items[0].body, "Alex's text",
        "live archive-shaped claims are not presence attribution"
    );
    let mut author = room_message("alex", "valid-edit", "Alex's correction");
    author["replaces_id"] = json!("original");
    author["_author_stamp"] = json!("alex@example.org");
    author["timestamp"] = json!("2026-10-31T10:06:00Z");
    let snapshot = ingest(&mut store, json!([author]), false);
    assert_eq!(snapshot.items[0].body, "Alex's correction");
    assert!(snapshot.items[0].edited);
}

#[test]
fn verified_current_self_nickname_supplies_identity_for_own_live_edits() {
    let mut store = TimelineStore::new();
    let mut archived = room_message("david", "original", "My original text");
    archived["mam_id"] = json!("archive-original");
    archived["author_real_jid"] = json!(ACCOUNT);
    let _ = ingest(&mut store, json!([archived]), true);
    let mut edit = room_message("david", "edit", "My correction");
    edit["replaces_id"] = json!("original");
    edit["timestamp"] = json!("2026-10-31T10:05:00Z");
    let snapshot = ingest(&mut store, json!([edit]), false);
    assert_eq!(snapshot.items[0].body, "My correction");
    assert!(snapshot.items[0].own);
    assert_eq!(snapshot.items[0].author_id.as_deref(), Some(ACCOUNT));
}

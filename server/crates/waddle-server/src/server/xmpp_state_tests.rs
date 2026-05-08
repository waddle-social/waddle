use super::*;
use crate::db::MigrationRunner;
use crate::db::{actor::DbActor, Database};
use crate::permissions::{ObjectType, PermissionActor};
use kameo::actor::ActorRef;
use std::sync::Arc;
use waddle_xmpp::AppState;

async fn create_test_db() -> (Arc<Database>, ActorRef<DbActor>) {
    let db = Database::in_memory("test-xmpp-state")
        .await
        .expect("Failed to create test database");
    let db = Arc::new(db);

    let runner = MigrationRunner::global();
    runner.run(&db).await.expect("Failed to run migrations");

    let actor = kameo::spawn(DbActor::new((*db).clone()));
    (db, actor)
}

#[tokio::test]
async fn test_xmpp_state_creation() {
    let (db, actor) = create_test_db().await;
    let state = XmppAppState::new(
        "waddle.social".to_string(),
        Arc::clone(&db),
        actor,
        kameo::spawn(PermissionActor::new_for_tests(db)),
        None,
    );

    assert_eq!(state.domain(), "waddle.social");
}

#[tokio::test]
async fn test_parse_resource() {
    let obj = XmppAppState::parse_resource("space:penguin-club").expect("Failed to parse");
    assert_eq!(obj.object_type, ObjectType::Space);
    assert_eq!(obj.id, "penguin-club");

    let obj = XmppAppState::parse_resource("channel:general").expect("Failed to parse");
    assert_eq!(obj.object_type, ObjectType::Channel);
    assert_eq!(obj.id, "general");
}

#[tokio::test]
async fn test_parse_subject() {
    let subj = XmppAppState::parse_subject("user:user-abc123").expect("Failed to parse");
    assert_eq!(subj.id, "user-abc123");
    assert!(subj.relation.is_none());
}

#[tokio::test]
async fn test_parse_invalid_resource() {
    let result = XmppAppState::parse_resource("invalid");
    assert!(result.is_err());
}

#[tokio::test]
async fn test_parse_invalid_subject() {
    let result = XmppAppState::parse_subject("invalid");
    assert!(result.is_err());
}

#[tokio::test]
async fn test_private_xml_roundtrip_uses_actor_boundary() {
    let (db, actor) = create_test_db().await;
    let state = XmppAppState::new(
        "waddle.social".to_string(),
        Arc::clone(&db),
        actor,
        kameo::spawn(PermissionActor::new_for_tests(db)),
        None,
    );
    let jid: jid::BareJid = "alice@waddle.social".parse().expect("valid bare jid");

    state
        .set_private_xml(
            &jid,
            "urn:xmpp:test",
            "<prefs><theme>aether</theme></prefs>",
        )
        .await
        .expect("set private xml");

    let stored = state
        .get_private_xml(&jid, "urn:xmpp:test")
        .await
        .expect("get private xml");
    assert_eq!(
        stored.as_deref(),
        Some("<prefs><theme>aether</theme></prefs>")
    );
}

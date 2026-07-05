use std::sync::Arc;

use jid::BareJid;
use kameo::actor::{ActorRef, Spawn};
use waddle_xmpp::muc::affiliation::DurableMembershipSource;

use super::PermissionDurableMembershipSource;
use crate::db::{Database, MigrationRunner};
use crate::permissions::{
    Object, ObjectType, PermissionActor, Relation, Subject, SubjectType, Tuple, WriteTuple,
};

async fn spawn_test_permission_actor(label: &str) -> ActorRef<PermissionActor> {
    let db = Database::in_memory(label).await.expect("in-memory db");
    let db = Arc::new(db);
    MigrationRunner::global()
        .run(&db)
        .await
        .expect("migrations");
    PermissionActor::spawn(PermissionActor::new_for_tests(db))
}

async fn write_tuple(
    actor: &ActorRef<PermissionActor>,
    object: Object,
    relation: &str,
    subject: Subject,
) {
    actor
        .ask(WriteTuple {
            tuple: Tuple::new(object, Relation::new(relation), subject),
        })
        .await
        .expect("write tuple");
}

fn bare(value: &str) -> BareJid {
    value.parse().expect("valid bare JID")
}

/// #1135: the production membership source must return every user
/// durably affiliated at Member+ — channel-level AND space-level
/// owner/admin/member relations — excluding channel outcasts and
/// non-user / userset subjects, sorted and deduplicated.
#[tokio::test]
async fn lists_channel_and_space_member_tier_users_excluding_outcasts() {
    let actor = spawn_test_permission_actor("durable-membership-source").await;
    let channel = Object::new(ObjectType::Channel, "c-1");
    let space = Object::new(ObjectType::Space, "w-1");

    // Channel-level member; also space member so dedup is exercised.
    write_tuple(
        &actor,
        channel.clone(),
        "member",
        Subject::user("alice@example.com"),
    )
    .await;
    write_tuple(
        &actor,
        space.clone(),
        "member",
        Subject::user("alice@example.com"),
    )
    .await;
    // Space-level admin counts as Member+.
    write_tuple(
        &actor,
        space.clone(),
        "admin",
        Subject::user("bob@example.com"),
    )
    .await;
    // Channel-level owner counts as Member+.
    write_tuple(
        &actor,
        channel.clone(),
        "owner",
        Subject::user("dave@example.com"),
    )
    .await;
    // Channel outcast wins over their member tuple.
    write_tuple(
        &actor,
        channel.clone(),
        "member",
        Subject::user("carol@example.com"),
    )
    .await;
    write_tuple(
        &actor,
        channel.clone(),
        "outcast",
        Subject::user("carol@example.com"),
    )
    .await;
    // Userset subject must not be treated as a member JID.
    write_tuple(
        &actor,
        channel.clone(),
        "member",
        Subject::userset(SubjectType::Space, "w-1", "member"),
    )
    .await;
    // A member of an unrelated channel must not leak in.
    write_tuple(
        &actor,
        Object::new(ObjectType::Channel, "c-other"),
        "member",
        Subject::user("erin@example.com"),
    )
    .await;

    let source = PermissionDurableMembershipSource::new(actor);
    let members = source
        .list_durable_member_jids("w-1", "c-1")
        .await
        .expect("list durable members");

    assert_eq!(
        members,
        vec![
            bare("alice@example.com"),
            bare("bob@example.com"),
            bare("dave@example.com"),
        ],
        "channel+space Member+ users, outcast-excluded, sorted, deduped"
    );
}

//! Outcast precedence on channel permissions.
//!
//! A channel `outcast` is denied every channel permission (`read`,
//! `send_message`, `view`, `moderate`, `manage` and `delete`), whether the
//! subject holds a direct channel relation or reaches the channel through its
//! parent Space. Message permissions that go through the channel follow,
//! including a moderator's deletion of another author's message. A message
//! author's own `edit` and `delete` derive from the `author` relation and
//! subtract the channel's `outcast` explicitly, so a banned author loses them
//! while the room's moderators can still remove the author's messages.
//!
//! Lifting a ban, by deleting the `outcast` relation, restores what the
//! subject's other relations grant.
//!
//! The offline tests pin the built-in SpiceDB schema and the local backend's
//! mirror of it. The `local_backend_` tests evaluate the matrix with the local
//! test backend over an in-memory database; a local tuple store establishes
//! neither SpiceDB nor cross-store correctness. The `live_spicedb_` tests
//! evaluate the same matrix with the production backend against a disposable
//! SpiceDB, and are the evidence for production behaviour.
#![cfg(feature = "test-support")]

use std::sync::Arc;

use kameo::actor::{ActorRef, Spawn};
use prescience::{Client, RelationshipFilter};
use tokio::sync::Mutex;
use waddle_server::config::SpiceDbConfig;
use waddle_server::db::{Database, MigrationRunner};
use waddle_server::permissions::{
    builtin_authz_schema, guarded_resource_definitions, CheckPermission, ComputedPermission,
    DeleteTuple, EnsureSchema, Object, ObjectType, Permission, PermissionActor, PermissionSchema,
    Relation, Subject, SubjectType, Tuple, WriteTuple, AUTHZ_CODEC_MARKER_DEFINITION,
    AUTHZ_SCHEMA_MARKER_DEFINITION,
};
use waddle_server::ServerConfig;

/// Every channel permission, in schema order.
fn channel_permissions() -> [Permission; 6] {
    [
        Permission::Delete,
        Permission::Manage,
        Permission::Moderate,
        Permission::SendMessage,
        Permission::Read,
        Permission::View,
    ]
}

/// Each direct channel relation and the channel permissions it grants when
/// the subject is not an outcast.
fn direct_grants() -> Vec<(&'static str, Vec<Permission>)> {
    use Permission::{Manage, Moderate, Read, SendMessage, View};
    vec![
        ("owner", vec![Manage, Moderate, SendMessage, Read, View]),
        ("admin", vec![Manage, Moderate, SendMessage, Read, View]),
        ("member", vec![SendMessage, Read, View]),
        ("manager", vec![Manage, Moderate, SendMessage, Read, View]),
        ("moderator", vec![Moderate, SendMessage, Read, View]),
        ("writer", vec![SendMessage, Read, View]),
        ("viewer", vec![Read, View]),
    ]
}

/// Each parent Space relation and the channel permissions it grants through
/// `parent->...` when the subject is not an outcast.
fn space_grants() -> Vec<(&'static str, Vec<Permission>)> {
    use Permission::{Delete, Manage, Moderate, Read, SendMessage, View};
    vec![
        ("member", vec![SendMessage, Read, View]),
        ("moderator", vec![Moderate]),
        ("admin", vec![Manage, Moderate, Delete]),
    ]
}

/// The lines of one definition of the built-in SpiceDB schema, trimmed.
fn definition_lines(name: &str) -> Vec<&'static str> {
    let header = format!("definition {name} {{");
    let mut lines = builtin_authz_schema().lines().map(str::trim);
    assert!(
        lines.any(|line| line == header),
        "built-in schema has no {name} definition"
    );
    lines.take_while(|line| *line != "}").collect()
}

#[test]
fn builtin_schema_subtracts_outcast_from_every_channel_permission() {
    let channel = definition_lines("channel");
    assert!(channel.contains(&"relation outcast: user"));
    for permission in channel_permissions() {
        let prefix = format!("permission {} = ", permission.as_str());
        let matching: Vec<&str> = channel
            .iter()
            .copied()
            .filter(|line| line.starts_with(&prefix))
            .collect();
        let [line] = matching.as_slice() else {
            panic!("channel declares {} {} times", prefix, matching.len());
        };
        let expression = &line[prefix.len()..];
        // The whole union, including every `parent->...` arrow, is inside the
        // parentheses, so the subtraction applies to all of it.
        let granted = expression
            .strip_prefix('(')
            .and_then(|rest| rest.strip_suffix(") - outcast"))
            .unwrap_or_else(|| panic!("{line} does not subtract outcast from all its grants"));
        assert!(!granted.contains('('), "{line}");
        assert!(!granted.contains('-'), "{line}");
    }
}

#[test]
fn builtin_schema_subtracts_outcast_from_author_edit_and_delete() {
    let message = definition_lines("message");
    for line in [
        "relation channel: channel",
        "relation author: user",
        "permission read = channel->read",
        "permission edit = author - channel->outcast",
        "permission delete = (author - channel->outcast) + channel->moderate",
        "permission react = channel->read",
    ] {
        assert!(
            message.contains(&line),
            "message lacks {line:?}: {message:?}"
        );
    }
}

#[test]
fn local_schema_mirrors_outcast_precedence() {
    let schema = PermissionSchema::default();
    for permission in channel_permissions() {
        let computation = schema
            .permission(ObjectType::Channel, permission.as_str())
            .unwrap_or_else(|| panic!("local channel schema lacks {}", permission.as_str()));
        let ComputedPermission::Exclusion(_, subtracted) = computation else {
            panic!(
                "local channel {} does not subtract anything: {computation:?}",
                permission.as_str()
            );
        };
        assert_eq!(**subtracted, ComputedPermission::direct("outcast"));
    }
    let author_unless_outcast = ComputedPermission::exclusion(
        ComputedPermission::direct("author"),
        ComputedPermission::arrow("channel", "outcast"),
    );
    assert_eq!(
        schema.permission(ObjectType::Message, "edit"),
        Some(&author_unless_outcast)
    );
    assert_eq!(
        schema.permission(ObjectType::Message, "delete"),
        Some(&ComputedPermission::union(vec![
            author_unless_outcast.clone(),
            ComputedPermission::arrow("channel", "moderate"),
        ]))
    );
}

async fn write(
    actor: &ActorRef<PermissionActor>,
    object: &Object,
    relation: &str,
    subject: Subject,
) {
    actor
        .ask(WriteTuple {
            tuple: Tuple::new(object.clone(), Relation::new(relation), subject),
        })
        .await
        .unwrap_or_else(|error| panic!("write {object}#{relation}: {error:?}"));
}

async fn delete(
    actor: &ActorRef<PermissionActor>,
    object: &Object,
    relation: &str,
    subject: Subject,
) {
    actor
        .ask(DeleteTuple {
            tuple: Tuple::new(object.clone(), Relation::new(relation), subject),
        })
        .await
        .unwrap_or_else(|error| panic!("delete {object}#{relation}: {error:?}"));
}

async fn allowed(
    actor: &ActorRef<PermissionActor>,
    subject: &Subject,
    permission: &Permission,
    object: &Object,
) -> bool {
    actor
        .ask(CheckPermission {
            subject: subject.clone(),
            permission: permission.clone(),
            object: object.clone(),
        })
        .await
        .unwrap_or_else(|error| panic!("check {} on {object}: {error:?}", permission.as_str()))
        .allowed
}

fn parent(space: &Object) -> Subject {
    Subject::userset(SubjectType::Space, space.id.clone(), "")
}

fn channel_of(channel: &Object) -> Subject {
    Subject::userset(SubjectType::Channel, channel.id.clone(), "")
}

fn unique_tag() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// An outcast who also holds a direct channel relation is denied every
/// channel permission. The grant alone is checked first, so the denial is
/// not vacuous.
async fn outcast_overrides_direct_channel_grants(actor: &ActorRef<PermissionActor>) {
    let tag = unique_tag();
    for (relation, granted) in direct_grants() {
        let channel = Object::new(ObjectType::Channel, format!("room-{relation}-{tag}"));
        let user = Subject::user(format!("{relation}-{tag}@example.org"));
        write(actor, &channel, relation, user.clone()).await;
        for permission in &granted {
            assert!(
                allowed(actor, &user, permission, &channel).await,
                "{relation} should hold {} before the ban",
                permission.as_str()
            );
        }

        write(actor, &channel, "outcast", user.clone()).await;
        for permission in channel_permissions() {
            assert!(
                !allowed(actor, &user, &permission, &channel).await,
                "outcast {relation} still holds {}",
                permission.as_str()
            );
        }
    }
}

/// An outcast who reaches the channel through its parent Space is denied
/// every channel permission, including the Space admin's `delete`.
async fn outcast_overrides_parent_space_grants(actor: &ActorRef<PermissionActor>) {
    let tag = unique_tag();
    for (relation, granted) in space_grants() {
        let space = Object::new(ObjectType::Space, format!("space-{relation}-{tag}"));
        let channel = Object::new(ObjectType::Channel, format!("room-{relation}-{tag}"));
        let user = Subject::user(format!("space-{relation}-{tag}@example.org"));
        write(actor, &channel, "parent", parent(&space)).await;
        write(actor, &space, relation, user.clone()).await;
        for permission in &granted {
            assert!(
                allowed(actor, &user, permission, &channel).await,
                "Space {relation} should hold channel {} before the ban",
                permission.as_str()
            );
        }

        write(actor, &channel, "outcast", user.clone()).await;
        for permission in channel_permissions() {
            assert!(
                !allowed(actor, &user, &permission, &channel).await,
                "outcast Space {relation} still holds channel {}",
                permission.as_str()
            );
        }
        // The ban is scoped to the room: Space permissions are unchanged.
        if relation == "admin" {
            assert!(allowed(actor, &user, &Permission::Update, &space).await);
        }
    }
}

/// A banned author loses `edit` and `delete` on their own messages in the
/// room, as well as what the message inherits from the channel, while a
/// moderator who is not banned can still delete those messages.
async fn outcast_author_loses_edit_and_delete(actor: &ActorRef<PermissionActor>) {
    let tag = unique_tag();
    let channel = Object::new(ObjectType::Channel, format!("room-author-{tag}"));
    let message = Object::new(ObjectType::Message, format!("message-author-{tag}"));
    let author = Subject::user(format!("author-{tag}@example.org"));
    let moderator = Subject::user(format!("room-moderator-{tag}@example.org"));
    write(actor, &message, "channel", channel_of(&channel)).await;
    write(actor, &message, "author", author.clone()).await;
    write(actor, &channel, "member", author.clone()).await;
    write(actor, &channel, "moderator", moderator.clone()).await;
    for permission in [
        Permission::Edit,
        Permission::Delete,
        Permission::Read,
        Permission::React,
    ] {
        assert!(
            allowed(actor, &author, &permission, &message).await,
            "author should hold message {} before the ban",
            permission.as_str()
        );
    }

    write(actor, &channel, "outcast", author.clone()).await;
    for permission in [
        Permission::Edit,
        Permission::Delete,
        Permission::Read,
        Permission::React,
    ] {
        assert!(
            !allowed(actor, &author, &permission, &message).await,
            "outcast author still holds message {}",
            permission.as_str()
        );
    }
    assert!(allowed(actor, &moderator, &Permission::Delete, &message).await);
    assert!(!allowed(actor, &moderator, &Permission::Edit, &message).await);
}

/// A moderator's deletion of another author's message derives from channel
/// `moderate`, so a banned moderator loses it while the author, not banned,
/// keeps it.
async fn banned_moderator_cannot_delete_others_messages(actor: &ActorRef<PermissionActor>) {
    let tag = unique_tag();
    let channel = Object::new(ObjectType::Channel, format!("room-moderated-{tag}"));
    let message = Object::new(ObjectType::Message, format!("message-moderated-{tag}"));
    let author = Subject::user(format!("writer-{tag}@example.org"));
    let moderator = Subject::user(format!("moderator-{tag}@example.org"));
    write(actor, &message, "channel", channel_of(&channel)).await;
    write(actor, &message, "author", author.clone()).await;
    write(actor, &channel, "moderator", moderator.clone()).await;
    assert!(allowed(actor, &moderator, &Permission::Delete, &message).await);
    assert!(!allowed(actor, &moderator, &Permission::Edit, &message).await);

    write(actor, &channel, "outcast", moderator.clone()).await;
    assert!(!allowed(actor, &moderator, &Permission::Delete, &message).await);
    assert!(allowed(actor, &author, &Permission::Delete, &message).await);
}

/// Lifting a ban restores exactly what the subject's other relations grant:
/// the outcast relation subtracts, it does not revoke the grants beside it.
/// Covers a direct channel grant, a parent Space grant and an author's own
/// message permissions.
async fn removing_outcast_restores_access(actor: &ActorRef<PermissionActor>) {
    let tag = unique_tag();
    let space = Object::new(ObjectType::Space, format!("space-unban-{tag}"));
    let channel = Object::new(ObjectType::Channel, format!("room-unban-{tag}"));
    let message = Object::new(ObjectType::Message, format!("message-unban-{tag}"));
    let member = Subject::user(format!("member-unban-{tag}@example.org"));
    let space_admin = Subject::user(format!("space-admin-unban-{tag}@example.org"));
    write(actor, &channel, "parent", parent(&space)).await;
    write(actor, &channel, "member", member.clone()).await;
    write(actor, &space, "admin", space_admin.clone()).await;
    write(actor, &message, "channel", channel_of(&channel)).await;
    write(actor, &message, "author", member.clone()).await;

    let member_channel = [Permission::SendMessage, Permission::Read, Permission::View];
    let admin_channel = [Permission::Manage, Permission::Moderate, Permission::Delete];
    let member_message = [
        Permission::Edit,
        Permission::Delete,
        Permission::Read,
        Permission::React,
    ];

    write(actor, &channel, "outcast", member.clone()).await;
    write(actor, &channel, "outcast", space_admin.clone()).await;
    for permission in channel_permissions() {
        assert!(!allowed(actor, &member, &permission, &channel).await);
        assert!(!allowed(actor, &space_admin, &permission, &channel).await);
    }
    for permission in &member_message {
        assert!(!allowed(actor, &member, permission, &message).await);
    }

    delete(actor, &channel, "outcast", member.clone()).await;
    delete(actor, &channel, "outcast", space_admin.clone()).await;
    for permission in &member_channel {
        assert!(
            allowed(actor, &member, permission, &channel).await,
            "unbanned member should hold channel {}",
            permission.as_str()
        );
    }
    for permission in &admin_channel {
        assert!(
            allowed(actor, &space_admin, permission, &channel).await,
            "unbanned Space admin should hold channel {}",
            permission.as_str()
        );
    }
    for permission in &member_message {
        assert!(
            allowed(actor, &member, permission, &message).await,
            "unbanned author should hold message {}",
            permission.as_str()
        );
    }
    // Lifting the ban grants nothing the other relations do not.
    assert!(!allowed(actor, &member, &Permission::Manage, &channel).await);
}

async fn local_backend() -> ActorRef<PermissionActor> {
    let db = Database::in_memory("channel-outcast-precedence")
        .await
        .expect("in-memory database");
    MigrationRunner::single()
        .run(&db)
        .await
        .expect("deployment migrations");
    let backend = PermissionActor::new_for_tests(Arc::new(db));
    assert_eq!(backend.backend_name(), "local");
    PermissionActor::spawn(backend)
}

#[tokio::test]
async fn local_backend_outcast_overrides_direct_channel_grants() {
    outcast_overrides_direct_channel_grants(&local_backend().await).await;
}

#[tokio::test]
async fn local_backend_outcast_overrides_parent_space_grants() {
    outcast_overrides_parent_space_grants(&local_backend().await).await;
}

#[tokio::test]
async fn local_backend_outcast_author_loses_edit_and_delete() {
    outcast_author_loses_edit_and_delete(&local_backend().await).await;
}

#[tokio::test]
async fn local_backend_banned_moderator_cannot_delete_others_messages() {
    banned_moderator_cannot_delete_others_messages(&local_backend().await).await;
}

#[tokio::test]
async fn local_backend_removing_outcast_restores_access() {
    removing_outcast_restores_access(&local_backend().await).await;
}

// Live tests share one SpiceDB with the other `spicedb-live` binaries, which
// empty it, so they run one at a time: this lock serializes them within the
// binary, and the nextest group serializes them across binaries. Each test
// uses identifiers of its own and empties the store before it starts.
static LIVE_SPICEDB: Mutex<()> = Mutex::const_new(());

/// The production backend against the disposable SpiceDB, with the built-in
/// schema of this build installed.
async fn live_backend() -> ActorRef<PermissionActor> {
    let endpoint =
        std::env::var("WADDLE_TEST_SPICEDB_ENDPOINT").expect("WADDLE_TEST_SPICEDB_ENDPOINT");
    let preshared_key = std::env::var("WADDLE_TEST_SPICEDB_PRESHARED_KEY")
        .expect("WADDLE_TEST_SPICEDB_PRESHARED_KEY");
    let raw = Client::builder(&endpoint, &preshared_key)
        .insecure(true)
        .build()
        .await
        .expect("raw SpiceDB client");
    // Without a schema-version marker, `EnsureSchema` installs this build's
    // schema whatever an earlier test left; without a foreign codec marker,
    // or relationships stored without a codec marker, it is not refused. A
    // definition the stored schema lacks, or every definition on a fresh
    // SpiceDB, makes its delete fail; that is the state wanted.
    for definition in guarded_resource_definitions().chain([
        AUTHZ_CODEC_MARKER_DEFINITION,
        AUTHZ_SCHEMA_MARKER_DEFINITION,
    ]) {
        let _ = raw
            .delete_relationships(RelationshipFilter::new(definition))
            .await;
    }
    let mut config = ServerConfig::test_homeserver();
    config.spicedb = Some(SpiceDbConfig {
        endpoint,
        preshared_key,
        insecure: true,
    });
    let backend = PermissionActor::from_server_config(&config)
        .await
        .expect("production SpiceDB backend");
    assert_eq!(backend.backend_name(), "spicedb");
    let actor = PermissionActor::spawn(backend);
    actor.ask(EnsureSchema).await.expect("built-in schema");
    actor
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT and WADDLE_TEST_SPICEDB_PRESHARED_KEY"]
async fn live_spicedb_outcast_overrides_direct_channel_grants() {
    let _serial = LIVE_SPICEDB.lock().await;
    outcast_overrides_direct_channel_grants(&live_backend().await).await;
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT and WADDLE_TEST_SPICEDB_PRESHARED_KEY"]
async fn live_spicedb_outcast_overrides_parent_space_grants() {
    let _serial = LIVE_SPICEDB.lock().await;
    outcast_overrides_parent_space_grants(&live_backend().await).await;
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT and WADDLE_TEST_SPICEDB_PRESHARED_KEY"]
async fn live_spicedb_outcast_author_loses_edit_and_delete() {
    let _serial = LIVE_SPICEDB.lock().await;
    outcast_author_loses_edit_and_delete(&live_backend().await).await;
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT and WADDLE_TEST_SPICEDB_PRESHARED_KEY"]
async fn live_spicedb_banned_moderator_cannot_delete_others_messages() {
    let _serial = LIVE_SPICEDB.lock().await;
    banned_moderator_cannot_delete_others_messages(&live_backend().await).await;
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT and WADDLE_TEST_SPICEDB_PRESHARED_KEY"]
async fn live_spicedb_removing_outcast_restores_access() {
    let _serial = LIVE_SPICEDB.lock().await;
    removing_outcast_restores_access(&live_backend().await).await;
}

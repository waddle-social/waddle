use std::str::FromStr;

use jid::{BareJid, FullJid};
use waddle_xmpp::muc::{
    AdminPresenceKind, DestroyReason, MucConfigStatusCode, MucOccupantNick, OccupantPresenceUpdate,
    OccupantVoiceChange, RoomLifecycleId, RoomMutationEffects, RoomRevision,
};
use waddle_xmpp::ownership::NodeIdentity;
use waddle_xmpp::{Role, Voice};

use super::*;
use crate::db::Database;

async fn store_with_db(name: &str) -> (Database, RoomEffectOutboxStore) {
    let database = Database::in_memory(name).await.expect("database");
    let store = RoomEffectOutboxStore::new(database.clone())
        .await
        .expect("store");
    (database, store)
}

fn room_jid() -> BareJid {
    BareJid::from_str("room@conference.example.test").expect("room JID")
}

fn lifecycle() -> RoomLifecycleId {
    RoomLifecycleId::generate()
}

fn initial_revision() -> RoomRevision {
    RoomRevision::initial()
}

fn origin() -> RoomEffectOriginInstanceId {
    RoomEffectOriginInstanceId::new("origin-instance".to_owned()).expect("origin instance")
}

fn producing_node() -> RoomEffectProducingNode {
    RoomEffectProducingNode::from_node_identity(NodeIdentity::new("node-a", "epoch-a"))
}

fn full_jid(value: &str) -> FullJid {
    FullJid::from_str(value).expect("full JID")
}

fn nick(value: &str) -> MucOccupantNick {
    MucOccupantNick::new(value.to_owned()).expect("occupant nick")
}

fn config_effects() -> RoomMutationEffects {
    RoomMutationEffects::config(
        room_jid(),
        vec![MucConfigStatusCode::NonPrivacyConfigurationChange],
        vec![full_jid("alice@example.test/device")],
    )
}

fn admin_effects() -> RoomMutationEffects {
    RoomMutationEffects::admin(
        room_jid(),
        vec![OccupantPresenceUpdate {
            recipient: full_jid("alice@example.test/device"),
            occupant: full_jid("room@conference.example.test/alice"),
            nick: nick("alice"),
            kind: AdminPresenceKind::Kicked,
            actor: Some(BareJid::from_str("mod@example.test").expect("actor JID")),
            reason: Some(DestroyReason::new("cleanup".to_owned()).expect("reason")),
        }],
        vec![OccupantPresenceUpdate {
            recipient: full_jid("bob@example.test/device"),
            occupant: full_jid("room@conference.example.test/alice"),
            nick: nick("alice"),
            kind: AdminPresenceKind::RoleChanged(Role::Participant),
            actor: None,
            reason: None,
        }],
        vec![OccupantVoiceChange {
            session: full_jid("carol@example.test/device"),
            voice: Voice::Muted,
        }],
        vec![full_jid("bob@example.test/device")],
    )
}

mod store_queue;

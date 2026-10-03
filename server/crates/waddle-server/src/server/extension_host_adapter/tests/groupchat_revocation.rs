//! A refused send still leaves the bot occupancy it used.
use super::{direct_ingress, groupchat_ingress::GroupchatFixture};
use crate::{
    ingress::{commit::commit_race_gate::Registration, test_support::IngressFixture},
    ingress_uow::{ConfiguredPluginGrants, ExtensionGrantRepository},
    server::extension_host_adapter::{ExtensionHostAdapter, ExtensionHostAdapterError},
};
use std::{sync::Arc, time::Duration};
use waddle_xmpp::{
    muc::room_actor::{GetSnapshot, JoinAffiliationGrant, JoinWithAffiliation},
    Stanza,
};
use waddle_xmpp_core::xep0359::OriginId;
use xmpp_parsers::presence::Type;

/// `leftover`: an interrupted send left the bot in the room, so this send
/// finds it present instead of joining, and still leaves it.
async fn revoked_after_bot_planning(f: IngressFixture, leftover: bool) {
    let mut fixture = GroupchatFixture::new(&f).await;
    let bot = fixture.invocation().actor_jid;
    if leftover {
        let revision = fixture
            .actor
            .ask(GetSnapshot)
            .await
            .expect("snapshot")
            .admission_revision;
        let session = crate::occupancy_authority::ensure(
            fixture.adapter.state.deps.app_state.db_pool.global(),
            &bot,
            waddle_xmpp_core::OccupancySessionGeneration::mint(),
        )
        .await
        .expect("bot generation");
        fixture
            .actor
            .ask(JoinWithAffiliation {
                sender_jid: bot.clone(),
                nick: "leftover".into(),
                affiliation_grant: JoinAffiliationGrant::HostOwned,
                local_domain: "example.com".into(),
                admission_revision: revision,
                session,
            })
            .await
            .expect("leftover occupancy");
    }
    let origin = OriginId::new(uuid::Uuid::new_v4().to_string());
    let gate = Registration::before_admission(origin.clone());
    let adapter = ExtensionHostAdapter::new(Arc::clone(&fixture.adapter.state));
    let invocation = fixture.invocation();
    let request = fixture.request(origin.as_str());
    let sending = tokio::spawn(async move { adapter.send_message(&invocation, request).await });
    tokio::time::timeout(Duration::from_secs(5), gate.entered())
        .await
        .expect("planning completed before admission transaction opens");
    let joined = fixture
        .actor
        .ask(GetSnapshot)
        .await
        .expect("joined snapshot");
    assert!(
        joined.room.session_generation(&bot).is_some(),
        "bot present"
    );
    let mut tx = f.uow.begin().await.expect("revocation transaction");
    assert_eq!(
        ExtensionGrantRepository::sync_configured(&mut tx, &[])
            .await
            .expect("revoke plugin grants")
            .revoked,
        2
    );
    tx.commit().await.expect("durable revocation");
    gate.release();
    let result = tokio::time::timeout(Duration::from_secs(5), sending)
        .await
        .expect("refused send completes")
        .expect("send task");
    assert!(
        matches!(result, Err(ExtensionHostAdapterError::NotAuthorized)),
        "{result:?}"
    );
    fixture.settle().await;
    assert_eq!(f.count("ingress_messages").await, 0);
    assert_eq!(f.count("extension_bot_rooms").await, 0, "nothing posted");
    let after = fixture
        .actor
        .ask(GetSnapshot)
        .await
        .expect("refusal snapshot");
    assert!(
        after.room.session_generation(&bot).is_none(),
        "the refused send leaves the bot occupancy it used"
    );
    let wire = fixture.drain();
    let types: Vec<_> = super::groupchat_ingress::bot_presences(&wire)
        .into_iter()
        .map(|(type_, ..)| type_)
        .collect();
    if leftover {
        assert_eq!(types, [Type::Unavailable], "no join, only the leave");
    } else {
        assert_eq!(types, [Type::None, Type::Unavailable], "join then leave");
    }
    assert!(!wire.iter().any(|stanza| matches!(stanza, Stanza::Message(message) if message.type_ == xmpp_parsers::message::MessageType::Groupchat)), "refused message cannot be delivered");

    let mut tx = f.uow.begin().await.expect("regrant transaction");
    ExtensionGrantRepository::sync_configured(
        &mut tx,
        &[ConfiguredPluginGrants {
            plugin: direct_ingress::plugin(),
            can_send: true,
            provider_rooms: vec![fixture.room.clone()],
        }],
    )
    .await
    .expect("regrant plugin");
    tx.commit().await.expect("durable regrant");
    fixture
        .send("after-regrant")
        .await
        .expect("regranted send succeeds");
    let regrant_wire = fixture.drain();
    super::groupchat_ingress::groupchat_message(&regrant_wire);
    assert_eq!(
        super::groupchat_ingress::bot_presences(&regrant_wire).len(),
        2,
        "a fresh join and leave"
    );
    assert!(fixture
        .actor
        .ask(GetSnapshot)
        .await
        .expect("regrant snapshot")
        .room
        .session_generation(&bot)
        .is_none());
    assert_eq!(f.count("ingress_messages").await, 1);
    assert_eq!(
        f.count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    fixture.close(f).await;
}

#[tokio::test]
async fn extension_groupchat_revocation_leaves_join_sqlite() {
    revoked_after_bot_planning(IngressFixture::sqlite().await, false).await;
}

#[tokio::test]
async fn extension_groupchat_revocation_leaves_join_postgres() {
    if let Some(f) = IngressFixture::postgres("bot_join_revoke").await {
        revoked_after_bot_planning(f, false).await;
    }
}

#[tokio::test]
async fn extension_groupchat_revocation_leaves_leftover_occupancy_sqlite() {
    revoked_after_bot_planning(IngressFixture::sqlite().await, true).await;
}

#[tokio::test]
async fn extension_groupchat_revocation_leaves_leftover_occupancy_postgres() {
    if let Some(f) = IngressFixture::postgres("bot_reuse_revoke").await {
        revoked_after_bot_planning(f, true).await;
    }
}

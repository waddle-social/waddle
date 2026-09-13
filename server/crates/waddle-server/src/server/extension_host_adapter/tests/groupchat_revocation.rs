//! Grant refusal compensates only the occupancy created by this dispatch.
use super::{direct_ingress, groupchat_ingress::GroupchatFixture};
use crate::{
    ingress::{commit::commit_race_gate::Registration, test_support::IngressFixture},
    ingress_uow::{ConfiguredPluginGrants, ExtensionGrantRepository},
    server::extension_host_adapter::{ExtensionHostAdapter, ExtensionHostAdapterError},
};
use std::{sync::Arc, time::Duration};
use waddle_xmpp::{muc::room_actor::GetSnapshot, Stanza};
use waddle_xmpp_core::xep0359::OriginId;
use xmpp_parsers::presence::Type;

async fn revoked_after_bot_planning(f: IngressFixture, reuse: bool) {
    let mut fixture = GroupchatFixture::new(&f).await;
    let bot = fixture.invocation().actor_jid;
    if reuse {
        fixture
            .send("existing-occupancy")
            .await
            .expect("initial send");
        fixture.drain();
    }
    let previous_rows = i64::from(reuse);
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
    let joined_session = joined.room.session_generation(&bot).expect("bot joined");
    assert_eq!(f.count("ingress_messages").await, previous_rows);
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
    assert_eq!(f.count("ingress_messages").await, previous_rows);
    let after = fixture
        .actor
        .ask(GetSnapshot)
        .await
        .expect("refusal snapshot");
    let wire = fixture.drain();
    let presences: Vec<_> = wire
        .iter()
        .filter_map(|stanza| match stanza {
            Stanza::Presence(presence) => Some(presence),
            _ => None,
        })
        .collect();
    if reuse {
        assert_eq!(after.room.session_generation(&bot), Some(joined_session));
        assert_eq!(after.occupancy_revision, joined.occupancy_revision);
        assert!(presences.is_empty(), "reused occupancy must not be parted");
    } else {
        assert!(
            after.room.session_generation(&bot).is_none(),
            "refused admission must remove this call's bot join"
        );
        assert_eq!(
            presences.len(),
            2,
            "join followed by compensating unavailable"
        );
        assert_eq!(presences[0].type_, Type::None);
        assert_eq!(presences[1].type_, Type::Unavailable);
        assert_eq!(presences[0].from, presences[1].from);
        assert_eq!(presences[0].to, presences[1].to);
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
    let rejoined = fixture
        .actor
        .ask(GetSnapshot)
        .await
        .expect("regrant snapshot");
    let rejoined_session = rejoined
        .room
        .session_generation(&bot)
        .expect("bot present after regrant");
    if reuse {
        assert_eq!(rejoined_session, joined_session);
    } else {
        assert_ne!(rejoined_session, joined_session, "regrant must join fresh");
    }
    let regrant_wire = fixture.drain();
    super::groupchat_ingress::groupchat_message(&regrant_wire);
    assert_eq!(
        regrant_wire
            .iter()
            .filter(|stanza| matches!(stanza, Stanza::Presence(_)))
            .count(),
        usize::from(!reuse)
    );
    assert_eq!(f.count("ingress_messages").await, previous_rows + 1);
    assert_eq!(
        f.count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        previous_rows + 1
    );
    fixture.close(f).await;
}

#[tokio::test]
async fn extension_groupchat_revocation_compensates_join_sqlite() {
    revoked_after_bot_planning(IngressFixture::sqlite().await, false).await;
}

#[tokio::test]
async fn extension_groupchat_revocation_compensates_join_postgres() {
    if let Some(f) = IngressFixture::postgres("bot_join_revoke").await {
        revoked_after_bot_planning(f, false).await;
    }
}

#[tokio::test]
async fn extension_groupchat_revocation_preserves_reused_occupancy_sqlite() {
    revoked_after_bot_planning(IngressFixture::sqlite().await, true).await;
}

#[tokio::test]
async fn extension_groupchat_revocation_preserves_reused_occupancy_postgres() {
    if let Some(f) = IngressFixture::postgres("bot_reuse_revoke").await {
        revoked_after_bot_planning(f, true).await;
    }
}

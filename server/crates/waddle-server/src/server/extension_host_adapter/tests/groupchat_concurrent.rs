use super::{direct_ingress, groupchat_ingress::GroupchatFixture};
use crate::{
    ingress::test_support::IngressFixture,
    ingress_uow::{ConfiguredPluginGrants, ExtensionGrantRepository},
    server::routes::interpret,
};
use std::{
    sync::{atomic::Ordering, Arc},
    time::Duration,
};
use waddle_extensions::PluginId;
use waddle_xmpp::{muc::room_actor::GetSnapshot, Stanza};

async fn two_bots(f: IngressFixture) {
    let fixture = GroupchatFixture::new(&f).await;
    let other = PluginId::new("other-bot").expect("plugin");
    let mut tx = f.uow.begin().await.expect("transaction");
    ExtensionGrantRepository::sync_configured(
        &mut tx,
        &[direct_ingress::plugin(), other.clone()]
            .into_iter()
            .map(|plugin| ConfiguredPluginGrants {
                plugin,
                can_send: true,
                provider_rooms: vec![fixture.room.clone()],
            })
            .collect::<Vec<_>>(),
    )
    .await
    .expect("grants");
    tx.commit().await.expect("commit");
    fixture.send("bot-a").await.expect("first bot");
    let mut invocation = fixture.invocation();
    invocation.plugin_id = other;
    invocation.actor_jid = fixture
        .adapter
        .plugin_actor_jid(&invocation.plugin_id)
        .expect("bot actor");
    fixture
        .adapter
        .send_message(&invocation, fixture.request("bot-b"))
        .await
        .expect("second bot");
    assert_eq!(
        f.count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        2,
        "both bot routes have complete receipts"
    );
    fixture.close(f).await;
}

async fn concurrent(f: IngressFixture) {
    let mut fixture = GroupchatFixture::new(&f).await;
    let gate = Arc::new(interpret::BotSnapshotGate::default());
    let spawn_send = |origin: &str| {
        let adapter = super::super::ExtensionHostAdapter::new(Arc::clone(&fixture.adapter.state));
        let invocation = fixture.invocation();
        let request = fixture.request(origin);
        let gate = Arc::clone(&gate);
        tokio::spawn(interpret::TEST_BOT_SNAPSHOT_GATE.scope(gate, async move {
            adapter.send_message(&invocation, request).await
        }))
    };
    let first = spawn_send("concurrent-a");
    gate.reached.notified().await;
    let second = spawn_send("concurrent-b");
    assert!(
        tokio::time::timeout(Duration::from_millis(150), gate.reached.notified())
            .await
            .is_err(),
        "second send must wait for the first bot lifecycle snapshot"
    );
    gate.release.notify_one();
    first.await.expect("task").expect("first committed");
    gate.reached.notified().await;
    gate.release.notify_one();
    second
        .await
        .expect("task")
        .expect("second committed without stale room generation");
    assert_eq!(gate.arrivals.load(Ordering::SeqCst), 2);
    let snapshot = fixture.actor.ask(GetSnapshot).await.expect("snapshot");
    assert_eq!(
        snapshot
            .room
            .occupants
            .values()
            .filter(|occupant| occupant.real_jid == fixture.invocation().actor_jid)
            .count(),
        1
    );
    assert_eq!(
        fixture
            .drain()
            .iter()
            .filter(|stanza| matches!(stanza, Stanza::Presence(_)))
            .count(),
        1,
        "only one join presence"
    );
    assert_eq!(
        f.count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        2
    );
    fixture.close(f).await;
}

#[tokio::test]
async fn extension_groupchat_two_bots_sqlite() {
    two_bots(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn extension_groupchat_two_bots_postgres() {
    if let Some(f) = IngressFixture::postgres("two_bots").await {
        two_bots(f).await;
    }
}
#[tokio::test]
async fn extension_groupchat_concurrent_sqlite() {
    concurrent(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn extension_groupchat_concurrent_postgres() {
    if let Some(f) = IngressFixture::postgres("concurrent_bot").await {
        concurrent(f).await;
    }
}

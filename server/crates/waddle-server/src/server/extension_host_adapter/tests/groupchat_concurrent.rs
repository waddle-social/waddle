use super::{direct_ingress, groupchat_ingress::GroupchatFixture};
use crate::{
    ingress::{
        nested::{TestGate, TEST_BEFORE_SETTLEMENT},
        test_support::IngressFixture,
    },
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

/// One bot's sends into one room run one at a time: the next send cannot
/// race this send's admission. A send queued behind another reuses its
/// lingering occupancy, so occupants see one join, both messages, one leave.
async fn concurrent(f: IngressFixture) {
    let mut fixture = GroupchatFixture::new(&f).await;
    let gate = Arc::new(interpret::BotSnapshotGate::default());
    let settlement_gate = Arc::new(TestGate::default());
    let spawn_send = |origin: &str, hold_settlement: bool| {
        let adapter = super::super::ExtensionHostAdapter::new(Arc::clone(&fixture.adapter.state));
        let invocation = fixture.invocation();
        let request = fixture.request(origin);
        let gate = Arc::clone(&gate);
        let settlement_gate = Arc::clone(&settlement_gate);
        tokio::spawn(interpret::TEST_BOT_SNAPSHOT_GATE.scope(gate, async move {
            let send = adapter.send_message(&invocation, request);
            if hold_settlement {
                TEST_BEFORE_SETTLEMENT.scope(settlement_gate, send).await
            } else {
                send.await
            }
        }))
    };
    let first = spawn_send("concurrent-a", true);
    gate.reached.notified().await;
    let second = spawn_send("concurrent-b", false);
    assert!(
        tokio::time::timeout(Duration::from_millis(150), gate.reached.notified())
            .await
            .is_err(),
        "second send must wait for the first bot lifecycle snapshot"
    );
    gate.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), settlement_gate.reached.notified())
        .await
        .expect("first send reached settlement after commit");
    assert_eq!(f.count("ingress_messages").await, 1);
    assert!(
        tokio::time::timeout(Duration::from_millis(150), gate.reached.notified())
            .await
            .is_err(),
        "second send waits until the first send is done"
    );
    // The first send replies at its response deadline while its settlement
    // still runs; the bot's next message still waits for that work.
    tokio::time::timeout(Duration::from_secs(5), first)
        .await
        .expect("first send replies at its response deadline")
        .expect("task")
        .expect("first committed");
    assert!(
        tokio::time::timeout(Duration::from_millis(150), gate.reached.notified())
            .await
            .is_err(),
        "second send waits for the first send's settlement"
    );
    settlement_gate.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), gate.reached.notified())
        .await
        .expect("second send plans once the first is delivered");
    gate.release.notify_one();
    second.await.expect("task").expect("second committed");
    fixture.settle().await;
    assert_eq!(gate.arrivals.load(Ordering::SeqCst), 2);
    let snapshot = fixture.actor.ask(GetSnapshot).await.expect("snapshot");
    assert!(snapshot
        .room
        .find_occupant_by_real_jid(&fixture.invocation().actor_jid)
        .is_none());
    use xmpp_parsers::{message::MessageType, presence::Type};
    let wire: Vec<_> = fixture
        .drain()
        .into_iter()
        .filter_map(|stanza| match stanza {
            Stanza::Presence(presence) => Some(format!("{:?}", presence.type_)),
            Stanza::Message(message) if message.type_ == MessageType::Groupchat => {
                message.id.map(|id| id.0).or(Some("groupchat".to_owned()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        wire,
        [
            format!("{:?}", Type::None),
            "concurrent-a".to_owned(),
            "concurrent-b".to_owned(),
            format!("{:?}", Type::Unavailable),
        ],
        "the queued send reused the occupancy"
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

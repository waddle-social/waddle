//! Real host dispatch with a persistent local room and durable recipients.
use super::super::*;
use super::direct_ingress;
use crate::ingress::test_support::IngressFixture;
use crate::ingress_uow::{ConfiguredPluginGrants, ExtensionGrantRepository};
use std::time::Duration;
use tokio::sync::mpsc;
use waddle_xmpp::{
    muc::{
        room_actor::{ChangeAffiliation, Join},
        room_registry_actor::CreateRoom,
        RoomConfig,
    },
    registry::OutboundStanza,
    Affiliation, Role, Stanza,
};

pub(super) struct GroupchatFixture {
    pub adapter: ExtensionHostAdapter,
    pub room: BareJid,
    pub actor: ActorRef<RoomActor>,
    pub receiver: mpsc::Receiver<OutboundStanza>,
}

pub(super) fn groupchat_message(wire: &[Stanza]) -> &xmpp_parsers::message::Message {
    let mut groupchat = None;
    for stanza in wire {
        let Stanza::Message(message) = stanza else {
            continue;
        };
        if message.type_ == xmpp_parsers::message::MessageType::Groupchat {
            assert!(groupchat.is_none(), "exactly one occupant groupchat copy");
            groupchat = Some(message);
        } else {
            assert_eq!(message.type_, xmpp_parsers::message::MessageType::Headline);
            assert!(
                message
                    .payloads
                    .iter()
                    .any(|payload| payload.is("push", waddle_xmpp::xep::xep0430::NS_WADDLE_INBOX)),
                "auxiliary message is the projected inbox update"
            );
        }
    }
    groupchat.expect("one occupant groupchat copy")
}

impl GroupchatFixture {
    pub async fn new(f: &IngressFixture) -> Self {
        let adapter = direct_ingress::adapter(f).await;
        let room: BareJid = "extension-room@muc.example.com".parse().expect("room");
        let actor = adapter
            .state
            .deps
            .protocol
            .room_registry
            .ask(CreateRoom {
                room_jid: room.clone(),
                waddle_id: "extension-space".into(),
                channel_id: "extension-room".into(),
                config: RoomConfig {
                    persistent: true,
                    members_only: true,
                    ..Default::default()
                },
            })
            .await
            .expect("persistent room");
        let live: FullJid = "romeo@example.com/web".parse().expect("live recipient");
        let (sender, receiver) = mpsc::channel(32);
        crate::server::routes::websocket::tests::register_test_connection(
            &adapter.state,
            &live,
            sender,
        )
        .await;
        for recipient in [
            live.to_bare(),
            "juliet@example.com".parse().expect("offline recipient"),
        ] {
            actor
                .ask(ChangeAffiliation {
                    jid: recipient,
                    affiliation: Affiliation::Member,
                })
                .await
                .expect("durable member");
        }
        actor
            .ask(Join {
                nick: "romeo".into(),
                real_jid: live,
                role: Role::Participant,
                affiliation: Affiliation::Member,
            })
            .await
            .expect("live member joins");
        let mut tx = f.uow.begin().await.expect("grant transaction");
        ExtensionGrantRepository::sync_configured(
            &mut tx,
            &[ConfiguredPluginGrants {
                plugin: direct_ingress::plugin(),
                can_send: true,
                provider_rooms: vec![room.clone()],
            }],
        )
        .await
        .expect("provider room grant");
        tx.commit().await.expect("grant commit");
        Self {
            adapter,
            room,
            actor,
            receiver,
        }
    }

    pub fn invocation(&self) -> ExtensionInvocation {
        ExtensionInvocation {
            session: None,
            actor_jid: self
                .adapter
                .plugin_actor_jid(&direct_ingress::plugin())
                .expect("plugin actor"),
            plugin_id: direct_ingress::plugin(),
            source_room: Some(self.room.clone()),
            kind: InvocationKind::ProviderWebhook,
            provider_room_grants: vec![self.room.clone()],
        }
    }

    pub fn request(&self, origin: &str) -> HostSendMessage {
        HostSendMessage {
            target: HostMessageTarget::Room(self.room.clone()),
            ..direct_ingress::request(origin)
        }
    }

    pub async fn send(&self, origin: &str) -> Result<StanzaId, ExtensionHostAdapterError> {
        self.adapter
            .send_message(&self.invocation(), self.request(origin))
            .await
    }

    pub fn drain(&mut self) -> Vec<Stanza> {
        let mut result = Vec::new();
        while let Ok(outbound) = self.receiver.try_recv() {
            result.push(outbound.stanza);
        }
        result
    }

    pub async fn close(self, f: IngressFixture) {
        assert!(
            self.adapter
                .state
                .deps
                .protocol
                .ingress
                .drain_and_join(Duration::from_secs(10))
                .await
        );
        drop(self);
        f.close().await;
    }
}

async fn two_sends_reuse_occupancy(f: IngressFixture) {
    let mut fixture = GroupchatFixture::new(&f).await;
    fixture.send("groupchat-first").await.expect("first send");
    let first = fixture
        .actor
        .ask(GetSnapshot)
        .await
        .expect("first snapshot");
    let bot = fixture.invocation().actor_jid;
    let session = first
        .room
        .session_generation(&bot)
        .expect("bot occupancy session");
    let first_wire = fixture.drain();
    groupchat_message(&first_wire);
    assert_eq!(
        first_wire
            .iter()
            .filter(|s| matches!(s, Stanza::Presence(_)))
            .count(),
        1,
        "one initial bot join presence"
    );
    fixture
        .send("groupchat-second")
        .await
        .expect("second send must reuse bot occupancy");
    let second = fixture
        .actor
        .ask(GetSnapshot)
        .await
        .expect("second snapshot");
    assert_eq!(second.room.session_generation(&bot), Some(session));
    assert_eq!(second.occupancy_revision, first.occupancy_revision);
    let second_wire = fixture.drain();
    groupchat_message(&second_wire);
    assert!(!second_wire.iter().any(|s| matches!(s, Stanza::Presence(_))));
    assert_eq!(
        f.count("ingress_messages").await,
        2,
        "both real adapter sends are canonical ingress"
    );
    fixture.close(f).await;
}

async fn provider_grant_revocation(f: IngressFixture) {
    let fixture = GroupchatFixture::new(&f).await;
    fixture
        .send("provider-before-revoke")
        .await
        .expect("synced provider grant sends");
    let mut tx = f.uow.begin().await.expect("sync transaction");
    ExtensionGrantRepository::sync_configured(
        &mut tx,
        &[ConfiguredPluginGrants {
            plugin: direct_ingress::plugin(),
            can_send: true,
            provider_rooms: vec![],
        }],
    )
    .await
    .expect("remove configured room grant");
    tx.commit().await.expect("revocation commit");
    assert!(
        matches!(
            fixture.send("provider-after-revoke").await,
            Err(ExtensionHostAdapterError::NotAuthorized)
        ),
        "durable grant revocation overrides stale invocation room grants"
    );
    assert_eq!(f.count("ingress_messages").await, 1);
    fixture.close(f).await;
}

#[tokio::test]
async fn extension_groupchat_two_sends_sqlite() {
    two_sends_reuse_occupancy(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn extension_groupchat_two_sends_postgres() {
    if let Some(f) = IngressFixture::postgres("groupchat_two").await {
        two_sends_reuse_occupancy(f).await;
    }
}
#[tokio::test]
async fn extension_groupchat_provider_revocation_sqlite() {
    provider_grant_revocation(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn extension_groupchat_provider_revocation_postgres() {
    if let Some(f) = IngressFixture::postgres("groupchat_provider").await {
        provider_grant_revocation(f).await;
    }
}

async fn requester_uses_plugin_sender(f: IngressFixture) {
    let fixture = GroupchatFixture::new(&f).await;
    let session = crate::server::routes::websocket::tests::create_test_server_owner_session(
        &fixture.adapter.state,
        "romeo",
    )
    .await;
    let mut invocation = direct_ingress::invocation();
    invocation.session = Some(session);
    invocation.source_room = Some(fixture.room.clone());
    fixture
        .adapter
        .send_message(&invocation, fixture.request("requester-room"))
        .await
        .expect("requester sends through active plugin grant");
    assert_eq!(f.count("ingress_messages").await, 1);
    let sender = fixture
        .adapter
        .plugin_actor_jid(&invocation.plugin_id)
        .expect("plugin actor")
        .to_bare();
    assert_eq!(
        f.optional_text("SELECT sender_bare_jid FROM ingress_origin_aliases")
            .await,
        Some(sender.to_string())
    );
    assert_eq!(
        f.count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    fixture.close(f).await;
}
#[tokio::test]
async fn extension_groupchat_requester_sender_sqlite() {
    requester_uses_plugin_sender(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn extension_groupchat_requester_sender_postgres() {
    if let Some(f) = IngressFixture::postgres("groupchat_requester").await {
        requester_uses_plugin_sender(f).await;
    }
}

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
        Self::on(f, direct_ingress::adapter(f).await).await
    }

    /// The same persistent room and live member on `adapter`'s node.
    pub async fn on(f: &IngressFixture, adapter: ExtensionHostAdapter) -> Self {
        Self::with_config(f, adapter, RoomConfig::default()).await
    }

    /// The fixture's room as a group DM: a conversation between people.
    pub async fn group_dm(f: &IngressFixture) -> Self {
        let config = RoomConfig {
            group_dm: true,
            ..Default::default()
        };
        Self::with_config(f, direct_ingress::adapter(f).await, config).await
    }

    async fn with_config(
        f: &IngressFixture,
        adapter: ExtensionHostAdapter,
        config: RoomConfig,
    ) -> Self {
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
                    ..config
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
                session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
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

    /// One send, including the bot's detached leave.
    pub async fn send(&self, origin: &str) -> Result<StanzaId, ExtensionHostAdapterError> {
        let result = self
            .adapter
            .send_message(&self.invocation(), self.request(origin))
            .await;
        self.settle().await;
        result
    }

    /// Wait until the last send's bot has left: the leave holds the
    /// bot/room lock until it is done.
    pub async fn settle(&self) {
        let _done = tokio::time::timeout(
            Duration::from_secs(10),
            self.adapter
                .state
                .deps
                .protocol
                .extension_bot_rooms
                .lock(&direct_ingress::plugin(), &self.room),
        )
        .await
        .expect("the bot's leave finishes");
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

/// The presences the bot sent one occupant, in order: `(type, affiliation,
/// role, wears the Bot hat)`.
pub(super) fn bot_presences(
    wire: &[Stanza],
) -> Vec<(
    xmpp_parsers::presence::Type,
    Option<String>,
    Option<String>,
    bool,
)> {
    wire.iter()
        .filter_map(|stanza| match stanza {
            Stanza::Presence(presence) => Some(presence),
            _ => None,
        })
        .map(|presence| {
            let item = presence
                .payloads
                .iter()
                .find(|payload| payload.is("x", waddle_xmpp::muc::presence::NS_MUC_USER))
                .and_then(|x| x.get_child("item", waddle_xmpp::muc::presence::NS_MUC_USER));
            let hats = presence
                .payloads
                .iter()
                .find(|payload| payload.is("hats", waddle_xmpp::xep::xep0317::NS_HATS))
                .map(waddle_xmpp::xep::xep0317::parse_hats_element)
                .unwrap_or_default();
            (
                presence.type_.clone(),
                item.and_then(|item| item.attr("affiliation"))
                    .map(str::to_owned),
                item.and_then(|item| item.attr("role")).map(str::to_owned),
                hats == waddle_xmpp::xep::xep0317::HatSet::new()
                    .with_hat(waddle_xmpp::xep::xep0317::Hat::bot()),
            )
        })
        .collect()
}

/// XEP-0045 + XEP-0317: every send in a members-only room is the bot's
/// join (Bot hat, no affiliation), its message, then its unavailable. The
/// bot keeps no occupancy, affiliation or durable recipient, and the room
/// records it once for its bot listing.
async fn each_send_joins_and_leaves(f: IngressFixture) {
    use xmpp_parsers::presence::Type;
    let mut fixture = GroupchatFixture::new(&f).await;
    let bot = fixture.invocation().actor_jid;
    let mut generation = None;
    for origin in ["groupchat-first", "groupchat-second"] {
        fixture.send(origin).await.expect("send");
        let wire = fixture.drain();
        groupchat_message(&wire);
        let message_at = wire
            .iter()
            .position(|s| matches!(s, Stanza::Message(m) if m.type_ == xmpp_parsers::message::MessageType::Groupchat))
            .expect("message");
        let presence_at: Vec<_> = wire
            .iter()
            .enumerate()
            .filter(|(_, s)| matches!(s, Stanza::Presence(_)))
            .map(|(at, _)| at)
            .collect();
        assert_eq!(presence_at.len(), 2, "{origin}: {wire:?}");
        assert!(presence_at[0] < message_at && message_at < presence_at[1]);
        let none = Some("none".to_owned());
        assert_eq!(
            bot_presences(&wire),
            [
                (
                    Type::None,
                    none.clone(),
                    Some("participant".to_owned()),
                    true
                ),
                (Type::Unavailable, none, Some("none".to_owned()), true),
            ],
            "{origin}"
        );
        let room = fixture.actor.ask(GetSnapshot).await.expect("snapshot").room;
        assert!(room.find_occupant_by_real_jid(&bot).is_none(), "{origin}");
        assert_eq!(room.get_affiliation(&bot.to_bare()), Affiliation::None);
        let chain = fixture
            .actor
            .ask(waddle_xmpp::muc::room_actor::GetRoomSnapshot {
                sender_jid: bot.clone(),
            })
            .await
            .expect("chain snapshot");
        assert!(!chain.durable_recipient_bare_jids.contains(&bot.to_bare()));
        // Durable room joins commit only for the published generation (#1869);
        // a bot keeps one.
        let current = f
            .optional_text("SELECT generation FROM xmpp_occupancy_authority WHERE full_jid = 'direct-test@extensions.example.com/bot'")
            .await
            .expect("published bot generation");
        assert_eq!(*generation.get_or_insert(current.clone()), current);
    }
    assert_eq!(f.count("ingress_messages").await, 2);
    assert_eq!(
        f.count("extension_bot_rooms WHERE room_jid = 'extension-room@muc.example.com' AND plugin_id = 'direct-test'")
            .await,
        1,
        "two sends record the room once"
    );
    // Uninstall: the configured set no longer has the plugin.
    let mut tx = f.uow.begin().await.expect("sync transaction");
    ExtensionGrantRepository::sync_configured(&mut tx, &[])
        .await
        .expect("uninstall");
    tx.commit().await.expect("uninstall commit");
    assert_eq!(f.count("extension_bot_rooms").await, 0);
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

/// A group DM is a conversation between people: an extension command or send
/// into one is refused before the bot joins, takes an affiliation or posts.
async fn group_dm_refuses_the_bot(f: IngressFixture) {
    let mut fixture = GroupchatFixture::group_dm(&f).await;
    let bot = fixture.invocation().actor_jid;
    assert!(
        matches!(
            fixture.send("group-dm-bot").await,
            Err(ExtensionHostAdapterError::NotAuthorized)
        ),
        "the bot is not authorized to speak in a group DM"
    );
    let room = fixture.actor.ask(GetSnapshot).await.expect("snapshot").room;
    assert!(room.config.group_dm);
    assert!(
        room.session_generation(&bot).is_none(),
        "the bot never joined the group DM"
    );
    assert_eq!(
        room.get_affiliation(&bot.to_bare()),
        Affiliation::None,
        "the refusal leaves no Member affiliation behind"
    );
    assert_eq!(f.count("ingress_messages").await, 0);
    assert!(
        fixture.drain().is_empty(),
        "no bot join presence or message reached a member"
    );
    fixture.close(f).await;
}

#[tokio::test]
async fn extension_groupchat_group_dm_refused_sqlite() {
    group_dm_refuses_the_bot(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn extension_groupchat_group_dm_refused_postgres() {
    if let Some(f) = IngressFixture::postgres("groupchat_group_dm").await {
        group_dm_refuses_the_bot(f).await;
    }
}

#[tokio::test]
async fn extension_groupchat_each_send_joins_and_leaves_sqlite() {
    each_send_joins_and_leaves(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn extension_groupchat_each_send_joins_and_leaves_postgres() {
    if let Some(f) = IngressFixture::postgres("groupchat_two").await {
        each_send_joins_and_leaves(f).await;
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

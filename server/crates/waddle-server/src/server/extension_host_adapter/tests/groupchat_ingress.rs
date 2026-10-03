//! Real host dispatch with a persistent local room and durable recipients.
use super::super::*;
use super::{super::groupchat::BOT_LINGER, direct_ingress};
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

    /// One send; the bot lingers in the room afterwards.
    pub async fn post(&self, origin: &str) -> Result<StanzaId, ExtensionHostAdapterError> {
        self.adapter
            .send_message(&self.invocation(), self.request(origin))
            .await
    }

    /// One send, including the bot's leave once its linger passes.
    pub async fn send(&self, origin: &str) -> Result<StanzaId, ExtensionHostAdapterError> {
        let result = self.post(origin).await;
        self.settle().await;
        result
    }

    /// Let the bot's linger pass and wait until it has left.
    pub async fn settle(&self) {
        self.adapter
            .state
            .deps
            .protocol
            .extension_bot_rooms
            .settled(&direct_ingress::plugin(), &self.room)
            .await;
    }

    pub async fn bot_present(&self) -> bool {
        self.actor
            .ask(GetSnapshot)
            .await
            .expect("snapshot")
            .room
            .find_occupant_by_real_jid(&self.invocation().actor_jid)
            .is_some()
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

/// What one occupant saw of the bot, in order.
#[derive(Debug, PartialEq)]
pub(super) enum Seen {
    Join,
    /// A groupchat message, by id.
    Message(String),
    Leave,
}

pub(super) fn seen(wire: &[Stanza]) -> Vec<Seen> {
    wire.iter()
        .filter_map(|stanza| match stanza {
            Stanza::Presence(presence) if presence.type_ == xmpp_parsers::presence::Type::None => {
                Some(Seen::Join)
            }
            Stanza::Presence(_) => Some(Seen::Leave),
            Stanza::Message(message)
                if message.type_ == xmpp_parsers::message::MessageType::Groupchat =>
            {
                Some(Seen::Message(
                    message.id.clone().map(|id| id.0).unwrap_or_default(),
                ))
            }
            Stanza::Message(_) | Stanza::Iq(_) => None,
        })
        .collect()
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

/// XEP-0045 + XEP-0317: the bot joins a members-only room for a send (Bot
/// hat, no affiliation) and lingers. A send within [`BOT_LINGER`] of the
/// last reuses the occupancy; once the window passes with no send, the bot
/// leaves, and a later send joins again. The bot keeps no affiliation or
/// durable recipient, records no account activity, and the room records it
/// once for its bot listing.
async fn sends_share_a_lingering_occupancy(f: IngressFixture) {
    use xmpp_parsers::presence::Type;
    let mut fixture = GroupchatFixture::new(&f).await;
    let bot = fixture.invocation().actor_jid;
    let message = |id: &str| Seen::Message(id.to_owned());
    fixture.post("linger-first").await.expect("first send");
    assert_eq!(
        seen(&fixture.drain()),
        [Seen::Join, message("linger-first")]
    );
    let generation = || async {
        f.optional_text("SELECT generation FROM xmpp_occupancy_authority WHERE full_jid = 'direct-test@extensions.example.com/bot'")
            .await
            .expect("published bot generation")
    };
    let joined = generation().await;
    // Just inside the window: the bot stays for the second send.
    tokio::time::pause();
    tokio::time::advance(BOT_LINGER - Duration::from_secs(1)).await;
    tokio::time::resume();
    fixture.post("linger-second").await.expect("second send");
    // The first send's leave comes due and finds the second send.
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(2)).await;
    tokio::time::resume();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(fixture.bot_present().await, "the bot lingers");
    assert_eq!(seen(&fixture.drain()), [message("linger-second")]);
    fixture.settle().await;
    assert_eq!(seen(&fixture.drain()), [Seen::Leave]);
    assert!(!fixture.bot_present().await);

    fixture
        .send("linger-after")
        .await
        .expect("send after the window");
    let wire = fixture.drain();
    assert_eq!(
        seen(&wire),
        [Seen::Join, message("linger-after"), Seen::Leave],
        "after the window the bot joins again"
    );
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
        ]
    );
    let room = fixture.actor.ask(GetSnapshot).await.expect("snapshot").room;
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
    assert_eq!(generation().await, joined);
    assert_eq!(f.count("ingress_messages").await, 3);
    assert_eq!(
        f.count("notification_activity").await,
        0,
        "a bot's sends and leaves are no account activity"
    );
    assert_eq!(
        f.count("extension_bot_rooms WHERE room_jid = 'extension-room@muc.example.com' AND plugin_id = 'direct-test'")
            .await,
        1,
        "the room records the bot once"
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

/// A group DM whose room is not loaded is refused from its channel record:
/// the bot's send loads, claims and joins nothing.
async fn unloaded_group_dm_is_refused_before_loading(f: IngressFixture) {
    let adapter = direct_ingress::adapter(&f).await;
    let (room, invocation) =
        managed_room(&f, &adapter, waddle_xmpp::admin::CHANNEL_TYPE_GROUP_DM).await;
    let result = managed_send(&adapter, &invocation, &room, "unloaded-group-dm").await;
    assert!(
        matches!(result, Err(ExtensionHostAdapterError::NotAuthorized)),
        "{result:?}"
    );
    assert!(
        adapter
            .state
            .deps
            .protocol
            .room_registry
            .ask(waddle_xmpp::muc::room_registry_actor::GetRoom { room_jid: room })
            .await
            .expect("room lookup")
            .is_none(),
        "the refusal loads no room"
    );
    assert_eq!(f.count("ingress_messages").await, 0);
    close_node(adapter, f).await;
}

#[tokio::test]
async fn extension_groupchat_unloaded_group_dm_refused_sqlite() {
    unloaded_group_dm_is_refused_before_loading(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn extension_groupchat_unloaded_group_dm_refused_postgres() {
    if let Some(f) = IngressFixture::postgres("groupchat_unloaded_group_dm").await {
        unloaded_group_dm_is_refused_before_loading(f).await;
    }
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
async fn extension_groupchat_sends_share_a_lingering_occupancy_sqlite() {
    sends_share_a_lingering_occupancy(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn extension_groupchat_sends_share_a_lingering_occupancy_postgres() {
    if let Some(f) = IngressFixture::postgres("groupchat_linger").await {
        sends_share_a_lingering_occupancy(f).await;
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

/// A managed room nobody is in goes dormant and is evicted. A bot send loads
/// it again from its channel, as a member join does, instead of reporting
/// the room missing.
async fn send_loads_an_evicted_room(f: IngressFixture) {
    let adapter = direct_ingress::adapter(&f).await;
    let (room, invocation) = managed_room(&f, &adapter, "text").await;
    for origin in ["before-eviction", "after-eviction"] {
        managed_send(&adapter, &invocation, &room, origin)
            .await
            .unwrap_or_else(|error| panic!("{origin}: {error:?}"));
        adapter
            .state
            .deps
            .protocol
            .extension_bot_rooms
            .settled(&direct_ingress::plugin(), &room)
            .await;
        let counts =
            crate::server::session_janitors::sweep_dormant_rooms_once(&adapter.state).await;
        assert_eq!(counts.evicted, 1, "{origin}: the bot left the room dormant");
    }
    assert_eq!(f.count("ingress_messages").await, 2);
    close_node(adapter, f).await;
}

/// Room presence is in memory only. A restarted node restores the room from
/// durable state without the bot that lingered before the restart, so the
/// room goes dormant with no leave left to run.
async fn restart_drops_a_lingering_bot(f: IngressFixture) {
    use crate::server::routes::websocket::handlers::presence::{
        get_managed_channel_for_room, parse_room_jid_context,
    };
    let adapter = direct_ingress::adapter(&f).await;
    let (room, invocation) = managed_room(&f, &adapter, "text").await;
    managed_send(&adapter, &invocation, &room, "before-restart")
        .await
        .expect("send");
    let bot = &invocation.actor_jid;
    assert!(bot_in_room(&adapter, &room, bot).await, "the bot lingers");
    let restarted = direct_ingress::node(&f).await;
    let channel = get_managed_channel_for_room(&restarted.state, &room)
        .await
        .expect("channel lookup")
        .expect("managed channel");
    crate::server::routes::websocket::get_or_create_room_actor(
        &restarted.state,
        &room,
        channel.room_config(),
        parse_room_jid_context(&room).0,
        channel.id,
    )
    .await
    .expect("room restored from durable state");
    assert!(
        !bot_in_room(&restarted, &room, bot).await,
        "the lingering bot is not restored"
    );
    let counts = crate::server::session_janitors::sweep_dormant_rooms_once(&restarted.state).await;
    assert_eq!(counts.evicted, 1, "the restored room goes dormant");
    drop(restarted);
    close_node(adapter, f).await;
}

/// #1108: the dormancy sweep evicts the room between the bot's room lookup
/// and its join. The send looks the room up once more, loads it again, and
/// posts instead of failing.
async fn send_survives_a_racing_eviction(f: IngressFixture) {
    use crate::server::routes::interpret::{BotSnapshotGate, TEST_BOT_SNAPSHOT_GATE};
    let adapter = direct_ingress::adapter(&f).await;
    let (room, invocation) = managed_room(&f, &adapter, "text").await;
    let gate = Arc::new(BotSnapshotGate::default());
    let sending = {
        let sender = ExtensionHostAdapter::new(Arc::clone(&adapter.state));
        let (invocation, room) = (invocation.clone(), room.clone());
        tokio::spawn(TEST_BOT_SNAPSHOT_GATE.scope(Arc::clone(&gate), async move {
            managed_send(&sender, &invocation, &room, "racing-eviction").await
        }))
    };
    gate.reached.notified().await;
    let counts = crate::server::session_janitors::sweep_dormant_rooms_once(&adapter.state).await;
    assert_eq!(counts.evicted, 1, "the sweep evicts the room the bot found");
    gate.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), gate.reached.notified())
        .await
        .expect("the send looks the room up again");
    gate.release.notify_one();
    sending.await.expect("send task").expect("send");
    assert_eq!(
        gate.arrivals.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "one retry"
    );
    assert!(
        bot_in_room(&adapter, &room, &invocation.actor_jid).await,
        "the bot joined the reloaded room"
    );
    assert_eq!(f.count("ingress_messages").await, 1);
    adapter
        .state
        .deps
        .protocol
        .extension_bot_rooms
        .settled(&direct_ingress::plugin(), &room)
        .await;
    close_node(adapter, f).await;
}

/// Whether `adapter`'s node has `room` loaded with `bot` in it.
pub(super) async fn bot_in_room(
    adapter: &ExtensionHostAdapter,
    room: &BareJid,
    bot: &FullJid,
) -> bool {
    let Some(actor) = adapter
        .state
        .deps
        .protocol
        .room_registry
        .ask(waddle_xmpp::muc::room_registry_actor::GetRoom {
            room_jid: room.clone(),
        })
        .await
        .expect("room lookup")
    else {
        return false;
    };
    actor
        .ask(GetSnapshot)
        .await
        .expect("snapshot")
        .room
        .find_occupant_by_real_jid(bot)
        .is_some()
}

/// A managed channel's room, not loaded, that the plugin may post into, and
/// its invocation.
pub(super) async fn managed_room(
    f: &IngressFixture,
    adapter: &ExtensionHostAdapter,
    channel_type: &str,
) -> (BareJid, ExtensionInvocation) {
    let room: BareJid = "extension-room@muc.example.com".parse().expect("room");
    crate::server::xmpp_state::upsert_xmpp_channel(
        adapter.state.deps.app_state.db_pool.global_actor().clone(),
        &crate::server::xmpp_state::XmppChannelUpsert {
            id: "extension-room".into(),
            name: "Extension room".into(),
            description: None,
            channel_type: channel_type.into(),
            position: 0,
            is_default: false,
            pin_permission: Default::default(),
            members_only: false,
            public_room: true,
        },
    )
    .await
    .expect("managed channel");
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
    let invocation = ExtensionInvocation {
        session: None,
        actor_jid: adapter
            .plugin_actor_jid(&direct_ingress::plugin())
            .expect("plugin actor"),
        plugin_id: direct_ingress::plugin(),
        source_room: Some(room.clone()),
        kind: InvocationKind::ProviderWebhook,
        provider_room_grants: vec![room.clone()],
    };
    (room, invocation)
}

pub(super) async fn managed_send(
    adapter: &ExtensionHostAdapter,
    invocation: &ExtensionInvocation,
    room: &BareJid,
    origin: &str,
) -> Result<StanzaId, ExtensionHostAdapterError> {
    adapter
        .send_message(
            invocation,
            HostSendMessage {
                target: HostMessageTarget::Room(room.clone()),
                ..direct_ingress::request(origin)
            },
        )
        .await
}

pub(super) async fn close_node(adapter: ExtensionHostAdapter, f: IngressFixture) {
    assert!(
        adapter
            .state
            .deps
            .protocol
            .ingress
            .drain_and_join(Duration::from_secs(10))
            .await
    );
    drop(adapter);
    f.close().await;
}

#[tokio::test]
async fn extension_groupchat_restart_drops_a_lingering_bot_sqlite() {
    restart_drops_a_lingering_bot(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn extension_groupchat_restart_drops_a_lingering_bot_postgres() {
    if let Some(f) = IngressFixture::postgres("groupchat_restart").await {
        restart_drops_a_lingering_bot(f).await;
    }
}

#[tokio::test]
async fn extension_groupchat_send_survives_a_racing_eviction_sqlite() {
    send_survives_a_racing_eviction(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn extension_groupchat_send_survives_a_racing_eviction_postgres() {
    if let Some(f) = IngressFixture::postgres("groupchat_racing_eviction").await {
        send_survives_a_racing_eviction(f).await;
    }
}

#[tokio::test]
async fn extension_groupchat_send_loads_an_evicted_room_sqlite() {
    send_loads_an_evicted_room(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn extension_groupchat_send_loads_an_evicted_room_postgres() {
    if let Some(f) = IngressFixture::postgres("groupchat_evicted").await {
        send_loads_an_evicted_room(f).await;
    }
}

/// The room's bot listing names the bot before anyone sees it join: a
/// client that refetches the listing on the hatted join presence finds it.
async fn join_is_listed_before_it_is_seen(f: IngressFixture) {
    let fixture = GroupchatFixture::new(&f).await;
    // An occupant without a local socket gets the join over the node route.
    fixture
        .actor
        .ask(Join {
            session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            nick: "juliet".into(),
            real_jid: "juliet@example.com/phone".parse().expect("juliet"),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("juliet joins");
    let db = fixture
        .adapter
        .state
        .deps
        .app_state
        .db_pool
        .global()
        .clone();
    let room = fixture.room.clone();
    let (listed_tx, mut listed_rx) = mpsc::unbounded_channel();
    let route: crate::server::routes::interpret::TestJoinPresenceRoute =
        Arc::new(move |_occupant, _presence| {
            let (db, room, listed) = (db.clone(), room.clone(), listed_tx.clone());
            Box::pin(async move {
                let bots = crate::server::extension_bot_rooms::list(&db, &room)
                    .await
                    .expect("bot listing");
                let _ = listed.send(bots);
            })
        });
    crate::server::routes::interpret::TEST_JOIN_PRESENCE_ROUTE
        .scope(route, fixture.send("listed-on-join"))
        .await
        .expect("send");
    assert_eq!(
        listed_rx.recv().await,
        Some(vec![direct_ingress::plugin()]),
        "the join presence went out after the bot was listed"
    );
    fixture.close(f).await;
}

#[tokio::test]
async fn extension_groupchat_join_is_listed_before_it_is_seen_sqlite() {
    join_is_listed_before_it_is_seen(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn extension_groupchat_join_is_listed_before_it_is_seen_postgres() {
    if let Some(f) = IngressFixture::postgres("groupchat_listed_join").await {
        join_is_listed_before_it_is_seen(f).await;
    }
}

/// A send cancelled while its join presences route, after one route has
/// finished, still leaves: the leave never polls the finished route again.
async fn cancelled_join_routing_still_leaves(f: IngressFixture) {
    let fixture = GroupchatFixture::new(&f).await;
    // Occupants without a local socket get the join over the node route.
    for real_jid in ["juliet@example.com/phone", "mercutio@example.com/remote"] {
        let real_jid: FullJid = real_jid.parse().expect("occupant");
        fixture
            .actor
            .ask(Join {
                session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
                nick: real_jid.node().expect("node").to_string(),
                real_jid,
                role: Role::Participant,
                affiliation: Affiliation::Member,
            })
            .await
            .expect("occupant joins");
    }
    let release = Arc::new(tokio::sync::Notify::new());
    let (slow_tx, mut slow_rx) = mpsc::unbounded_channel();
    // Juliet's route finishes at once; mercutio's waits for the release.
    let route: crate::server::routes::interpret::TestJoinPresenceRoute = {
        let release = Arc::clone(&release);
        Arc::new(move |occupant: FullJid, _presence| {
            let (release, slow) = (Arc::clone(&release), slow_tx.clone());
            Box::pin(async move {
                if occupant
                    .node()
                    .is_some_and(|node| node.as_str() == "mercutio")
                {
                    let _ = slow.send(());
                    release.notified().await;
                }
            })
        })
    };
    let sending = {
        let adapter = fixture.adapter.clone();
        let (invocation, request) = (fixture.invocation(), fixture.request("cancelled-join"));
        tokio::spawn(
            crate::server::routes::interpret::TEST_JOIN_PRESENCE_ROUTE.scope(route, async move {
                adapter.send_message(&invocation, request).await
            }),
        )
    };
    slow_rx.recv().await.expect("the slow route starts");
    // Well inside the join presence cap: planning has consumed juliet's
    // finished route and still waits for mercutio's.
    tokio::time::sleep(Duration::from_millis(100)).await;
    sending.abort();
    assert!(sending.await.expect_err("cancelled").is_cancelled());
    assert!(fixture.bot_present().await, "the cancelled send joined");
    release.notify_one();
    fixture.settle().await;
    assert!(!fixture.bot_present().await, "the bot left");
    fixture.close(f).await;
}

#[tokio::test]
async fn extension_groupchat_cancelled_join_routing_still_leaves_sqlite() {
    cancelled_join_routing_still_leaves(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn extension_groupchat_cancelled_join_routing_still_leaves_postgres() {
    if let Some(f) = IngressFixture::postgres("groupchat_cancelled_join").await {
        cancelled_join_routing_still_leaves(f).await;
    }
}

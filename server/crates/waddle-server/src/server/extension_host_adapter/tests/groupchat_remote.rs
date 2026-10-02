//! #1893: an extension room send runs on the node that owns the room actor.
//!
//! Two server nodes share one database. Only the relay hop is replaced: the
//! request and reply cross the kameo codec, and the owner runs its real host
//! send path.
use super::{
    direct_ingress,
    groupchat_ingress::{groupchat_message, GroupchatFixture},
    groupchat_receipts,
};
use crate::{
    clustering::relay::{
        RelayAskError, RelayExtensionRoomSend, RelayExtensionRoomSendReply, RelaySendEffect,
        RelaySendFailure,
    },
    ingress::{commit::commit_race_gate::Registration, test_support::IngressFixture},
    ingress_uow::{ConfiguredPluginGrants, ExtensionGrantRepository},
    server::{
        extension_host_adapter::{
            relayed_room_send,
            remote_room::{TestRoomOwnerRelay, OWNER_REPLY_RESERVE, TEST_ROOM_OWNER_RELAY},
        },
        routes::{
            interpret::{BotSnapshotGate, PlanningClaims, TEST_BOT_SNAPSHOT_GATE},
            websocket::WebSocketState,
        },
    },
};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use waddle_extensions::{
    host_tools::{
        HostToolError, HostToolErrorCode, InvocationContext, InvocationKind, MessageTarget,
        SendMessageRequest,
    },
    DisplayText, PluginId, StanzaId,
};
use waddle_xmpp::{
    ingress::IngressEffectIntent,
    muc::room_actor::{ChangeAffiliation, GetSnapshot, Join},
    ownership::{NodeIdentity, SharedNodeIdentity},
    Affiliation, Role, Stanza,
};
use waddle_xmpp_core::xep0359::OriginId;

fn origin_node() -> NodeIdentity {
    NodeIdentity::new("origin", "epoch")
}

fn owner_node() -> NodeIdentity {
    NodeIdentity::new("owner", "epoch")
}

#[derive(Clone, Copy)]
enum Fault {
    /// The owner commits, then the reply is lost in transit.
    LoseReply,
    /// A peer that predates the relay message.
    OldPeer,
    /// The room claim moves to the origin before the owner plans.
    MoveToOrigin,
    /// The owner answers `NotOwner` without running.
    NotOwner,
    /// The connection drops while the owner is still planning; the owner
    /// keeps running and the re-ask arrives before it finishes.
    DropWhileRunning,
    /// The ask reaches the owner after the origin stopped waiting.
    ArrivesLate,
    /// The origin can wait only this much longer than the owner's reserve.
    Budget(Duration),
    /// The owner never answers.
    Silent,
}

struct Transport {
    owner: Arc<WebSocketState>,
    claims: Arc<PlanningClaims>,
    faults: Mutex<VecDeque<Fault>>,
    asks: AtomicUsize,
    gate: Arc<BotSnapshotGate>,
    detached: Mutex<Vec<tokio::task::JoinHandle<RelayExtensionRoomSendReply>>>,
}

impl Transport {
    async fn ask(
        &self,
        to: NodeIdentity,
        send: RelayExtensionRoomSend,
    ) -> Result<RelayExtensionRoomSendReply, RelayAskError> {
        assert_eq!(to, owner_node(), "the origin asks the claimed owner");
        self.asks.fetch_add(1, Ordering::SeqCst);
        let fault = self.faults.lock().expect("faults").pop_front();
        match fault {
            Some(Fault::OldPeer) => {
                return Err(RelayAskError::Send {
                    failure: RelaySendFailure::Codec,
                    effect: RelaySendEffect::NoEffect,
                    message: "peer does not know waddle.clustering.relay.extension_room_send.v1"
                        .into(),
                })
            }
            Some(Fault::NotOwner) => return Ok(RelayExtensionRoomSendReply::NotOwner),
            Some(Fault::MoveToOrigin) => self.claims.set_owner(origin_node()),
            Some(Fault::Silent) => std::future::pending::<()>().await,
            Some(
                Fault::LoseReply | Fault::DropWhileRunning | Fault::ArrivesLate | Fault::Budget(_),
            )
            | None => {}
        }
        let mut send: RelayExtensionRoomSend =
            rmp_serde::from_slice(&rmp_serde::to_vec_named(&send).expect("encode request"))
                .expect("decode request");
        assert!(
            send.origin_budget > Duration::ZERO,
            "the origin stamps its wait"
        );
        match fault {
            Some(Fault::ArrivesLate) => send.origin_budget = Duration::ZERO,
            Some(Fault::Budget(extra)) => send.origin_budget = OWNER_REPLY_RESERVE + extra,
            _ => {}
        }
        if matches!(fault, Some(Fault::DropWhileRunning)) {
            // Like kameo's delegated reply, the owner task outlives the ask.
            let first = tokio::spawn(TEST_BOT_SNAPSHOT_GATE.scope(
                Arc::clone(&self.gate),
                relayed_room_send(Arc::clone(&self.owner), send),
            ));
            self.gate.reached.notified().await;
            self.detached.lock().expect("detached").push(first);
            let gate = Arc::clone(&self.gate);
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(100)).await;
                gate.release.notify_one();
            });
            return Err(RelayAskError::Send {
                failure: RelaySendFailure::Transport,
                effect: RelaySendEffect::MaybeCommitted,
                message: "connection closed".into(),
            });
        }
        let reply = relayed_room_send(Arc::clone(&self.owner), send).await;
        let reply = rmp_serde::from_slice(&rmp_serde::to_vec_named(&reply).expect("encode reply"))
            .expect("decode reply");
        if matches!(fault, Some(Fault::LoseReply)) {
            return Err(RelayAskError::Send {
                failure: RelaySendFailure::ReplyTimeout,
                effect: RelaySendEffect::MaybeCommitted,
                message: "reply lost".into(),
            });
        }
        Ok(reply)
    }
}

struct Cluster {
    origin: GroupchatFixture,
    owner: GroupchatFixture,
    claims: Arc<PlanningClaims>,
}

fn join_cluster(fixture: &mut GroupchatFixture, claims: &Arc<PlanningClaims>, node: NodeIdentity) {
    let state = Arc::get_mut(&mut fixture.adapter.state).expect("unique websocket state");
    let app = Arc::get_mut(&mut state.deps.app_state).expect("unique app state");
    app.clustering_claims = crate::clustering::ClusteringHandles {
        claim_store: Some(Arc::clone(claims) as _),
        node_identity: Some(SharedNodeIdentity::new(node)),
        ..Default::default()
    };
}

impl Cluster {
    async fn new(f: &IngressFixture) -> Self {
        let claims = Arc::new(PlanningClaims::new(owner_node()));
        let mut origin = GroupchatFixture::new(f).await;
        let mut owner = GroupchatFixture::on(f, direct_ingress::node(f).await).await;
        join_cluster(&mut origin, &claims, origin_node());
        join_cluster(&mut owner, &claims, owner_node());
        Self {
            origin,
            owner,
            claims,
        }
    }

    fn transport(&self, faults: impl IntoIterator<Item = Fault>) -> Arc<Transport> {
        Arc::new(Transport {
            owner: Arc::clone(&self.owner.adapter.state),
            claims: Arc::clone(&self.claims),
            faults: Mutex::new(faults.into_iter().collect()),
            asks: AtomicUsize::new(0),
            gate: Arc::default(),
            detached: Mutex::default(),
        })
    }

    fn provider(&self, plugin: PluginId) -> InvocationContext {
        InvocationContext {
            waddle_id: waddle_extensions::WaddleId::new("extension-space").expect("waddle"),
            plugin_id: plugin,
            requester: None,
            source_room: Some(self.owner.room.clone()),
            kind: InvocationKind::ProviderWebhook,
            provider_room_grants: vec![self.owner.room.clone()],
        }
    }

    fn requester(&self, user: &str) -> InvocationContext {
        InvocationContext {
            requester: Some(user.parse().expect("requester")),
            kind: InvocationKind::Command,
            provider_room_grants: Vec::new(),
            ..self.provider(direct_ingress::plugin())
        }
    }

    fn request(&self) -> SendMessageRequest {
        SendMessageRequest {
            target: MessageTarget::Muc(self.owner.room.clone()),
            body: DisplayText::new("cross-node bot body").expect("body"),
            thread_id: None,
            reply_to: None,
            markup: Vec::new(),
            extensions: None,
        }
    }

    async fn send(
        &self,
        transport: &Arc<Transport>,
        context: &InvocationContext,
        offered: &str,
        request: SendMessageRequest,
    ) -> Result<StanzaId, HostToolError> {
        TEST_ROOM_OWNER_RELAY
            .scope(
                relay(transport),
                self.origin.adapter.send_host_message(
                    context,
                    request,
                    StanzaId::new(offered).expect("offered id"),
                ),
            )
            .await
    }

    fn bot(&self, plugin: &PluginId) -> jid::FullJid {
        self.origin.adapter.plugin_actor_jid(plugin).expect("bot")
    }

    async fn bot_joined(fixture: &GroupchatFixture, bot: &jid::FullJid) -> bool {
        fixture
            .actor
            .ask(GetSnapshot)
            .await
            .expect("room snapshot")
            .room
            .session_generation(bot)
            .is_some()
    }

    async fn close(self, f: IngressFixture) {
        assert!(
            self.owner
                .adapter
                .state
                .deps
                .protocol
                .ingress
                .drain_and_join(Duration::from_secs(10))
                .await
        );
        drop(self.owner);
        self.origin.close(f).await;
    }
}

fn relay(transport: &Arc<Transport>) -> TestRoomOwnerRelay {
    let transport = Arc::clone(transport);
    Arc::new(move |to, send| {
        let transport = Arc::clone(&transport);
        Box::pin(async move { transport.ask(to, send).await })
    })
}

fn presences(wire: &[Stanza]) -> Vec<&xmpp_parsers::presence::Presence> {
    wire.iter()
        .filter_map(|stanza| match stanza {
            Stanza::Presence(presence) => Some(presence),
            _ => None,
        })
        .collect()
}

fn has_groupchat(wire: &[Stanza]) -> bool {
    wire.iter().any(|stanza| matches!(stanza, Stanza::Message(message) if message.type_ == xmpp_parsers::message::MessageType::Groupchat))
}

async fn archived_id(f: &IngressFixture) -> String {
    groupchat_receipts::intents(f)
        .await
        .into_iter()
        .find_map(|intent| match intent {
            IngressEffectIntent::ArchiveAuthoritative { stanza_id, .. } => Some(stanza_id.id),
            _ => None,
        })
        .expect("canonical room archive id")
}

async fn runs_on_owner(f: IngressFixture, provider: bool) {
    let mut cluster = Cluster::new(&f).await;
    let context = if provider {
        cluster.provider(direct_ingress::plugin())
    } else {
        crate::server::routes::websocket::tests::create_test_server_owner_session(
            &cluster.owner.adapter.state,
            "romeo",
        )
        .await;
        cluster.requester("romeo@example.com")
    };
    let transport = cluster.transport([]);
    let id = cluster
        .send(&transport, &context, "remote-owned", cluster.request())
        .await
        .expect("the owner sends");
    assert_eq!(transport.asks.load(Ordering::SeqCst), 1);
    assert_eq!(f.count("ingress_messages").await, 1);
    assert_eq!(f.count("mam_messages").await, 1);
    assert_eq!(id.as_str(), archived_id(&f).await, "owner's canonical id");
    let wire = cluster.owner.drain();
    let message = groupchat_message(&wire);
    assert_eq!(
        waddle_xmpp_core::xep0359::extract_stanza_id_by(
            message,
            &cluster.owner.room.clone().into()
        )
        .as_deref(),
        Some(id.as_str())
    );
    assert_eq!(presences(&wire).len(), 1, "one bot join on the owner");
    assert!(cluster.origin.drain().is_empty(), "the origin runs nothing");
    let bot = cluster.bot(&direct_ingress::plugin());
    assert!(Cluster::bot_joined(&cluster.owner, &bot).await);
    assert!(!Cluster::bot_joined(&cluster.origin, &bot).await);
    assert_eq!(
        f.optional_text("SELECT sender_bare_jid FROM ingress_origin_aliases")
            .await,
        Some(bot.to_bare().to_string())
    );
    cluster.close(f).await;
}

/// The requester identity dedupes through the bot's origin alias too.
async fn lost_reply_reasks_same_offer(f: IngressFixture) {
    let mut cluster = Cluster::new(&f).await;
    crate::server::routes::websocket::tests::create_test_server_owner_session(
        &cluster.owner.adapter.state,
        "romeo",
    )
    .await;
    let transport = cluster.transport([Fault::LoseReply]);
    let context = cluster.requester("romeo@example.com");
    let id = cluster
        .send(&transport, &context, "lost-reply", cluster.request())
        .await
        .expect("the re-ask resolves the committed send");
    assert_eq!(transport.asks.load(Ordering::SeqCst), 2);
    assert_eq!(f.count("ingress_messages").await, 1);
    assert_eq!(f.count("mam_messages").await, 1);
    assert_eq!(id.as_str(), archived_id(&f).await);
    let wire = cluster.owner.drain();
    groupchat_message(&wire);
    assert_eq!(presences(&wire).len(), 1, "no second join or fanout");
    cluster.close(f).await;
}

async fn not_owner_reresolves_once(f: IngressFixture) {
    let mut cluster = Cluster::new(&f).await;
    let context = cluster.provider(direct_ingress::plugin());
    let bot = cluster.bot(&direct_ingress::plugin());
    let moved = cluster.transport([Fault::MoveToOrigin]);
    let id = cluster
        .send(&moved, &context, "moved-room", cluster.request())
        .await
        .expect("the origin now owns the room");
    assert_eq!(moved.asks.load(Ordering::SeqCst), 1);
    assert!(
        cluster.owner.drain().is_empty(),
        "the stale owner ran nothing"
    );
    assert!(!Cluster::bot_joined(&cluster.owner, &bot).await);
    let wire = cluster.origin.drain();
    assert_eq!(
        waddle_xmpp_core::xep0359::extract_stanza_id_by(
            groupchat_message(&wire),
            &cluster.origin.room.clone().into()
        )
        .as_deref(),
        Some(id.as_str())
    );
    assert!(Cluster::bot_joined(&cluster.origin, &bot).await);
    assert_eq!(f.count("ingress_messages").await, 1);

    cluster.claims.set_owner(owner_node());
    let flapping = cluster.transport([Fault::NotOwner, Fault::NotOwner]);
    let error = cluster
        .send(&flapping, &context, "flapping-room", cluster.request())
        .await
        .expect_err("ownership keeps moving");
    assert_eq!(error.code, HostToolErrorCode::TemporaryFailure);
    assert_eq!(flapping.asks.load(Ordering::SeqCst), 2, "one re-resolve");
    assert_eq!(f.count("ingress_messages").await, 1);
    cluster.close(f).await;
}

async fn old_peer_is_temporary(f: IngressFixture) {
    let mut cluster = Cluster::new(&f).await;
    let transport = cluster.transport([Fault::OldPeer]);
    let context = cluster.provider(direct_ingress::plugin());
    let error = cluster
        .send(&transport, &context, "old-peer", cluster.request())
        .await
        .expect_err("an old peer cannot accept the send");
    assert_eq!(error.code, HostToolErrorCode::TemporaryFailure);
    assert_eq!(
        transport.asks.load(Ordering::SeqCst),
        1,
        "no-effect is final"
    );
    assert_eq!(f.count("ingress_messages").await, 0);
    assert!(cluster.owner.drain().is_empty());
    assert!(cluster.origin.drain().is_empty());
    let bot = cluster.bot(&direct_ingress::plugin());
    assert!(!Cluster::bot_joined(&cluster.owner, &bot).await);
    cluster.close(f).await;
}

async fn denials_cross_nodes(f: IngressFixture) {
    let mut cluster = Cluster::new(&f).await;
    let transport = cluster.transport([]);
    let bot = cluster.bot(&direct_ingress::plugin());
    let denied = |result: Result<StanzaId, HostToolError>| {
        assert_eq!(result.expect_err("denied").code, HostToolErrorCode::Denied);
    };

    // A requester without channel permission.
    denied(
        cluster
            .send(
                &transport,
                &cluster.requester("juliet@example.com"),
                "acl-denied",
                cluster.request(),
            )
            .await,
    );
    // An outcast bot.
    let outcast = |affiliation| ChangeAffiliation {
        jid: bot.to_bare(),
        affiliation,
    };
    cluster
        .owner
        .actor
        .ask(outcast(Affiliation::Outcast))
        .await
        .expect("outcast bot");
    let provider = cluster.provider(direct_ingress::plugin());
    denied(
        cluster
            .send(&transport, &provider, "outcast-bot", cluster.request())
            .await,
    );
    cluster
        .owner
        .actor
        .ask(outcast(Affiliation::None))
        .await
        .expect("lift outcast");
    assert!(!Cluster::bot_joined(&cluster.owner, &bot).await);
    assert!(cluster.owner.drain().is_empty());

    // A grant revoked after the owner joined the bot.
    let offered = "revoked-grant";
    let gate = Registration::before_admission(OriginId::new(offered.to_owned()));
    let sending = {
        let adapter = cluster.origin.adapter.clone();
        let request = cluster.request();
        tokio::spawn(TEST_ROOM_OWNER_RELAY.scope(relay(&transport), async move {
            adapter
                .send_host_message(
                    &provider,
                    request,
                    StanzaId::new(offered).expect("offered id"),
                )
                .await
        }))
    };
    tokio::time::timeout(Duration::from_secs(5), gate.entered())
        .await
        .expect("the owner planned and joined");
    assert!(Cluster::bot_joined(&cluster.owner, &bot).await);
    let mut tx = f.uow.begin().await.expect("revocation transaction");
    ExtensionGrantRepository::sync_configured(&mut tx, &[])
        .await
        .expect("revoke plugin grants");
    tx.commit().await.expect("durable revocation");
    gate.release();
    denied(
        tokio::time::timeout(Duration::from_secs(5), sending)
            .await
            .expect("refused send completes")
            .expect("send task"),
    );
    assert!(
        !Cluster::bot_joined(&cluster.owner, &bot).await,
        "the owner revokes this call's join"
    );
    let wire = cluster.owner.drain();
    let presences = presences(&wire);
    assert_eq!(presences.len(), 2, "join then compensating unavailable");
    assert_eq!(
        presences[1].type_,
        xmpp_parsers::presence::Type::Unavailable
    );
    assert!(!has_groupchat(&wire));
    assert_eq!(f.count("ingress_messages").await, 0);
    let mut tx = f.uow.begin().await.expect("regrant transaction");
    ExtensionGrantRepository::sync_configured(
        &mut tx,
        &[ConfiguredPluginGrants {
            plugin: direct_ingress::plugin(),
            can_send: true,
            provider_rooms: vec![cluster.owner.room.clone()],
        }],
    )
    .await
    .expect("regrant");
    tx.commit().await.expect("regrant commit");
    cluster.close(f).await;
}

/// An ask that reaches the owner after the origin stopped waiting is refused
/// before planning: nothing commits and the bot does not join.
async fn late_ask_commits_nothing(f: IngressFixture) {
    let mut cluster = Cluster::new(&f).await;
    let transport = cluster.transport([Fault::ArrivesLate]);
    let context = cluster.provider(direct_ingress::plugin());
    let error = cluster
        .send(&transport, &context, "late-owner", cluster.request())
        .await
        .expect_err("a late owner does not commit");
    assert_eq!(error.code, HostToolErrorCode::TemporaryFailure);
    assert_eq!(
        transport.asks.load(Ordering::SeqCst),
        1,
        "a refusal is final"
    );
    assert_eq!(f.count("ingress_messages").await, 0);
    assert_eq!(f.count("mam_messages").await, 0);
    let bot = cluster.bot(&direct_ingress::plugin());
    assert!(!Cluster::bot_joined(&cluster.owner, &bot).await);
    assert!(
        cluster.owner.drain().is_empty(),
        "the refusal precedes the bot join"
    );
    assert!(cluster.origin.drain().is_empty());
    cluster.close(f).await;
}

/// A deadline that passes during planning refuses the commit but keeps the
/// join, which is the occupancy every successful send leaves behind.
async fn deadline_during_planning_keeps_join(f: IngressFixture) {
    let mut cluster = Cluster::new(&f).await;
    let transport = cluster.transport([Fault::Budget(Duration::from_millis(200))]);
    let context = cluster.provider(direct_ingress::plugin());
    let gate = Arc::clone(&transport.gate);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(600)).await;
        gate.release.notify_one();
    });
    let error = TEST_BOT_SNAPSHOT_GATE
        .scope(
            Arc::clone(&transport.gate),
            cluster.send(&transport, &context, "slow-owner", cluster.request()),
        )
        .await
        .expect_err("the owner missed its deadline");
    assert_eq!(error.code, HostToolErrorCode::TemporaryFailure);
    assert_eq!(f.count("ingress_messages").await, 0);
    assert_eq!(f.count("mam_messages").await, 0);
    let bot = cluster.bot(&direct_ingress::plugin());
    assert!(Cluster::bot_joined(&cluster.owner, &bot).await);
    let wire = cluster.owner.drain();
    let presences = presences(&wire);
    assert_eq!(presences.len(), 1, "the join stays");
    assert_eq!(presences[0].type_, xmpp_parsers::presence::Type::None);
    assert!(!has_groupchat(&wire));
    cluster.close(f).await;
}

/// kameo bounds the mailbox and reply only on the owner. A silent owner must
/// not hold the origin past its own bound; the re-ask then succeeds.
async fn silent_owner_is_bounded_locally(f: IngressFixture) {
    let mut cluster = Cluster::new(&f).await;
    let transport = cluster.transport([Fault::Silent]);
    let context = cluster.provider(direct_ingress::plugin());
    let started = std::time::Instant::now();
    let id = tokio::time::timeout(
        Duration::from_secs(14),
        cluster.send(&transport, &context, "silent-owner", cluster.request()),
    )
    .await
    .expect("the origin bounds a silent owner")
    .expect("the re-ask succeeds");
    assert!(started.elapsed() >= Duration::from_secs(5));
    assert_eq!(transport.asks.load(Ordering::SeqCst), 2);
    assert_eq!(id.as_str(), archived_id(&f).await);
    groupchat_message(&cluster.owner.drain());
    cluster.close(f).await;
}

/// A re-ask that arrives while the first attempt still runs on the owner
/// shares that attempt's outcome instead of planning a second send.
async fn concurrent_reask_shares_first_attempt(f: IngressFixture) {
    let mut cluster = Cluster::new(&f).await;
    let transport = cluster.transport([Fault::DropWhileRunning]);
    let context = cluster.provider(direct_ingress::plugin());
    // A second planning pass would wait at the gate, which opens only once.
    let id = tokio::time::timeout(
        Duration::from_secs(10),
        TEST_BOT_SNAPSHOT_GATE.scope(
            Arc::clone(&transport.gate),
            cluster.send(&transport, &context, "concurrent-reask", cluster.request()),
        ),
    )
    .await
    .expect("the re-ask does not plan again")
    .expect("the re-ask returns the first attempt's id");
    assert_eq!(transport.asks.load(Ordering::SeqCst), 2);
    assert_eq!(transport.gate.arrivals.load(Ordering::SeqCst), 1);
    let first = transport
        .detached
        .lock()
        .expect("detached")
        .pop()
        .expect("first attempt");
    assert_eq!(
        first.await.expect("first attempt task"),
        RelayExtensionRoomSendReply::Sent(id.clone())
    );
    assert_eq!(f.count("ingress_messages").await, 1);
    assert_eq!(f.count("mam_messages").await, 1);
    assert_eq!(id.as_str(), archived_id(&f).await);
    let wire = cluster.owner.drain();
    groupchat_message(&wire);
    assert_eq!(presences(&wire).len(), 1, "one join, one fanout");
    cluster.close(f).await;
}

async fn signed_on_owner(f: IngressFixture) {
    let mut cluster = Cluster::new(&f).await;
    Arc::get_mut(&mut cluster.owner.adapter.state)
        .expect("unique owner state")
        .deps
        .protocol
        .extension_manager = super::groupchat_signed::signed_manager().await;
    let plugin = PluginId::new("message-hook-fixture").expect("fixture plugin");
    let mut tx = f.uow.begin().await.expect("grant tx");
    ExtensionGrantRepository::sync_configured(
        &mut tx,
        &[ConfiguredPluginGrants {
            plugin: plugin.clone(),
            can_send: true,
            provider_rooms: vec![cluster.owner.room.clone()],
        }],
    )
    .await
    .expect("fixture grant");
    tx.commit().await.expect("grant commit");
    let mut envelope =
        super::envelope_with_launch_room(Some(cluster.owner.room.to_string().as_str()));
    envelope.enrichments[0].plugin = plugin.clone();
    envelope.enrichments[0].payload_namespace =
        waddle_extensions::types::PayloadNamespace::new("urn:test:message-hook")
            .expect("namespace");
    envelope.enrichments[0].launches[0].plugin = plugin.clone();
    let mut request = cluster.request();
    request.extensions = Some(envelope);
    let transport = cluster.transport([Fault::LoseReply]);
    let id = cluster
        .send(
            &transport,
            &cluster.provider(plugin),
            "signed-remote",
            request,
        )
        .await
        .expect("signed send");
    assert_eq!(transport.asks.load(Ordering::SeqCst), 2);
    assert_eq!(id.as_str(), archived_id(&f).await);
    assert_eq!(f.count("ingress_messages").await, 1, "replay is idempotent");
    let wire = cluster.owner.drain();
    let launch = groupchat_message(&wire)
        .payloads
        .iter()
        .find(|p| p.is("extensions", waddle_extensions::FRAMEWORK_NAMESPACE))
        .and_then(|p| p.get_child("enrichment", waddle_extensions::FRAMEWORK_NAMESPACE))
        .and_then(|e| e.get_child("launch", waddle_extensions::FRAMEWORK_NAMESPACE))
        .expect("launch on the wire");
    assert!(
        launch.attr("token").is_some(),
        "the owner signed the launch"
    );
    cluster.close(f).await;
}

/// XEP-0045 §7.2.3: an occupant whose socket lives on another node has the
/// bot's join presence, with hat and occupant id, before the message can be
/// admitted and fanned out.
async fn join_presence_reaches_remote_occupant(f: IngressFixture) {
    let mut fixture = GroupchatFixture::new(&f).await;
    let remote: jid::FullJid = "mercutio@example.com/remote".parse().expect("remote");
    fixture
        .actor
        .ask(Join {
            session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            nick: "mercutio".into(),
            real_jid: remote.clone(),
            role: Role::Participant,
            affiliation: Affiliation::None,
        })
        .await
        .expect("remote-attached occupant");
    let routed = Arc::new(Mutex::new(Vec::new()));
    let offered = "remote-occupant-join";
    let gate = Registration::before_admission(OriginId::new(offered.to_owned()));
    let sending = {
        let adapter = fixture.adapter.clone();
        let invocation = fixture.invocation();
        let request = fixture.request(offered);
        let routed = Arc::clone(&routed);
        tokio::spawn(
            crate::server::routes::interpret::CONTROLLED_REGISTERED_REMOTE_DELIVERY.scope(
                (
                    crate::server::routes::interpret::FullJidDeliveryOutcome::Delivered,
                    routed,
                ),
                async move { adapter.send_message(&invocation, request).await },
            ),
        )
    };
    tokio::time::timeout(Duration::from_secs(5), gate.entered())
        .await
        .expect("the send reaches admission");
    {
        let routed = routed.lock().expect("routed");
        let [(target, Stanza::Presence(presence))] = routed.as_slice() else {
            panic!("only the join is routed before admission: {routed:?}");
        };
        assert_eq!(target, &remote);
        assert_eq!(presence.type_, xmpp_parsers::presence::Type::None);
        assert!(presence
            .payloads
            .iter()
            .any(|p| p.is("hats", waddle_xmpp::xep::xep0317::NS_HATS)));
        assert!(presence
            .payloads
            .iter()
            .any(|p| p.is("occupant-id", waddle_xmpp::xep::xep0421::NS_OCCUPANT_ID)));
    }
    gate.release();
    tokio::time::timeout(Duration::from_secs(5), sending)
        .await
        .expect("send completes")
        .expect("send task")
        .expect("send");
    let wire = fixture.drain();
    assert_eq!(presences(&wire).len(), 1, "local occupant too");
    groupchat_message(&wire);
    fixture.close(f).await;
}

#[tokio::test]
async fn extension_remote_requester_send_sqlite() {
    let f = IngressFixture::sqlite().await;
    runs_on_owner(f, false).await;
}
#[tokio::test]
async fn extension_remote_requester_send_postgres() {
    if let Some(f) = IngressFixture::postgres("extension_remote_requester_send").await {
        runs_on_owner(f, false).await;
    }
}

#[tokio::test]
async fn extension_remote_provider_send_sqlite() {
    let f = IngressFixture::sqlite().await;
    runs_on_owner(f, true).await;
}
#[tokio::test]
async fn extension_remote_provider_send_postgres() {
    if let Some(f) = IngressFixture::postgres("extension_remote_provider_send").await {
        runs_on_owner(f, true).await;
    }
}

#[tokio::test]
async fn extension_remote_lost_reply_reask_sqlite() {
    let f = IngressFixture::sqlite().await;
    lost_reply_reasks_same_offer(f).await;
}
#[tokio::test]
async fn extension_remote_lost_reply_reask_postgres() {
    if let Some(f) = IngressFixture::postgres("extension_remote_lost_reply_reask").await {
        lost_reply_reasks_same_offer(f).await;
    }
}

#[tokio::test]
async fn extension_remote_not_owner_reresolve_sqlite() {
    let f = IngressFixture::sqlite().await;
    not_owner_reresolves_once(f).await;
}
#[tokio::test]
async fn extension_remote_not_owner_reresolve_postgres() {
    if let Some(f) = IngressFixture::postgres("extension_remote_not_owner_reresolve").await {
        not_owner_reresolves_once(f).await;
    }
}

#[tokio::test]
async fn extension_remote_old_peer_sqlite() {
    let f = IngressFixture::sqlite().await;
    old_peer_is_temporary(f).await;
}
#[tokio::test]
async fn extension_remote_old_peer_postgres() {
    if let Some(f) = IngressFixture::postgres("extension_remote_old_peer").await {
        old_peer_is_temporary(f).await;
    }
}

#[tokio::test]
async fn extension_remote_denials_sqlite() {
    let f = IngressFixture::sqlite().await;
    denials_cross_nodes(f).await;
}
#[tokio::test]
async fn extension_remote_denials_postgres() {
    if let Some(f) = IngressFixture::postgres("extension_remote_denials").await {
        denials_cross_nodes(f).await;
    }
}

#[tokio::test]
async fn extension_remote_late_ask_sqlite() {
    let f = IngressFixture::sqlite().await;
    late_ask_commits_nothing(f).await;
}
#[tokio::test]
async fn extension_remote_late_ask_postgres() {
    if let Some(f) = IngressFixture::postgres("extension_remote_late_ask").await {
        late_ask_commits_nothing(f).await;
    }
}

#[tokio::test]
async fn extension_remote_deadline_during_planning_sqlite() {
    let f = IngressFixture::sqlite().await;
    deadline_during_planning_keeps_join(f).await;
}
#[tokio::test]
async fn extension_remote_deadline_during_planning_postgres() {
    if let Some(f) = IngressFixture::postgres("extension_remote_deadline_planning").await {
        deadline_during_planning_keeps_join(f).await;
    }
}

#[tokio::test]
async fn extension_remote_silent_owner_sqlite() {
    let f = IngressFixture::sqlite().await;
    silent_owner_is_bounded_locally(f).await;
}
#[tokio::test]
async fn extension_remote_silent_owner_postgres() {
    if let Some(f) = IngressFixture::postgres("extension_remote_silent_owner").await {
        silent_owner_is_bounded_locally(f).await;
    }
}

#[tokio::test]
async fn extension_remote_concurrent_reask_sqlite() {
    let f = IngressFixture::sqlite().await;
    concurrent_reask_shares_first_attempt(f).await;
}
#[tokio::test]
async fn extension_remote_concurrent_reask_postgres() {
    if let Some(f) = IngressFixture::postgres("extension_remote_concurrent_reask").await {
        concurrent_reask_shares_first_attempt(f).await;
    }
}

#[tokio::test]
async fn extension_remote_signed_launch_sqlite() {
    let f = IngressFixture::sqlite().await;
    signed_on_owner(f).await;
}
#[tokio::test]
async fn extension_remote_signed_launch_postgres() {
    if let Some(f) = IngressFixture::postgres("extension_remote_signed_launch").await {
        signed_on_owner(f).await;
    }
}

#[tokio::test]
async fn extension_bot_join_presence_remote_occupant_sqlite() {
    let f = IngressFixture::sqlite().await;
    join_presence_reaches_remote_occupant(f).await;
}
#[tokio::test]
async fn extension_bot_join_presence_remote_occupant_postgres() {
    if let Some(f) = IngressFixture::postgres("extension_bot_join_presence_remote_occupant").await {
        join_presence_reaches_remote_occupant(f).await;
    }
}

//! XEP-0045 groupchat in a room managed by an enrolled host-grant scope
//! (ADR-0020 D5, first slice): the audience of an accepted message.
//!
//! The room planner plans a groupchat for whoever the room actor and the
//! legacy stores name: the seated occupants, the accounts with a durable
//! room affiliation, the members of the parent Space, and the accounts a
//! message mentions. None of those is the authority of a managed room. The
//! acceptance transaction reads the canonical grants under the scope's guard
//! and accepts the plan only for the principals they admit: anyone else is
//! routed, projected and notified nothing, and does not keep the message
//! from the admitted audience.
//!
//! Every case here plans with the production planner: a real room actor
//! holds the occupancy and the legacy affiliations, the production durable
//! membership source reads the legacy Space membership from the permission
//! store, and `plan_message_dispatch` produces the plan that the production
//! commit path then accepts. The cases first assert that the planner did
//! plan for the principal the policy excludes, so that what they show is
//! the acceptance boundary and not an audience the planner happened to
//! leave out.
//!
//! They need PostgreSQL only. The fixture activates the scope in place of a
//! verified projection, as the other SQL admission suites do.
#![cfg(feature = "test-support")]

// The shared host-grant and ingress fixtures serve several suites; this one
// uses a subset of each.
#[allow(dead_code)]
mod host_grant_support;
#[allow(dead_code)]
mod ingress_support;

use std::sync::Arc;
use std::time::Duration;

use host_grant_support::{bare, GrantFixture, OWNER, PEER, PEER_RESOURCE, SENDER};
use jid::{BareJid, FullJid};
use kameo::actor::ActorRef;
use tokio::sync::mpsc::Receiver;
use waddle_server::db::{DatabaseConfig, DatabasePool, PoolConfig};
use waddle_server::ingress::commit::commit_submission;
use waddle_server::ingress::effects::direct::ExternalDirectEffect;
use waddle_server::ingress::effects::room::{DurableRoomEffect, ExternalRoomEffect};
use waddle_server::ingress::effects::{DurableEffect, Effect, ExternalEffect, PlannedEffect};
use waddle_server::ingress::execute::execute_effects;
use waddle_server::ingress::{
    ImmediateSink, IngressAuthority, IngressDecision, IngressDecisionClass, IngressSubmission,
    RecoveryEnvironment,
};
use waddle_server::notification_outbox::NotificationCandidate;
use waddle_server::permissions::host_grant::{Activation, RoomAffiliation};
use waddle_server::permissions::{Object, ObjectType, Relation, Subject, Tuple, WriteTuple};
use waddle_server::test_support::{
    bind_live_resource, create_test_session, websocket_state_with_ingress,
    PermissionDurableMembershipSource, WebSocketState,
};
use waddle_xmpp::ingress::{DigestContext, DigestInput, IngressEffectIntent, NormalizedTarget};
use waddle_xmpp::muc::room_actor::{ChangeAffiliation, HydrateDurableRecipients, Join, RoomActor};
use waddle_xmpp::muc::room_registry_actor::CreateRoom;
use waddle_xmpp::registry::OutboundStanza;
use waddle_xmpp::Stanza;
use xmpp_parsers::message::{Lang, Message, MessageType};

/// A member whose grant is revoked; holds a legacy room affiliation and is
/// offline.
const REVOKED: &str = "carol@example.com";
/// A member of the parent Space in the legacy permission store who was
/// never granted anything in the room; offline.
const SPACE_MEMBER: &str = "dana@example.com";
/// An account the room actor still seats and calls a member, with a live
/// connection, that holds no grant.
const STALE: &str = "erin@example.com";
const STALE_RESOURCE: &str = "erin@example.com/laptop";

/// A managed room as a running server holds it: the enrolled scope, a room
/// actor with occupants and legacy affiliations, and live connections.
struct ManagedRoomFixture {
    grants: GrantFixture,
    state: Arc<WebSocketState>,
    authority: Arc<IngressAuthority>,
    room: ActorRef<RoomActor>,
    sender: FullJid,
    /// The sender's live connection, kept open for the fixture's lifetime.
    sender_connection: Option<Receiver<OutboundStanza>>,
}

impl ManagedRoomFixture {
    /// The fixture scope's room with its sender and its peer seated, both
    /// holding a member grant and a legacy member affiliation.
    async fn start(label: &str) -> Option<Self> {
        let grants = GrantFixture::active(label).await?;
        let pool = Arc::new(
            DatabasePool::try_new(
                DatabaseConfig::new(grants.ingress.db.driver(), grants.ingress.db.database_url()),
                PoolConfig,
            )
            .await
            .expect("shared database pool"),
        );
        let authority = Arc::new(grants.ingress.authority().await);
        let state = websocket_state_with_ingress(pool, Arc::clone(&authority)).await;
        let room = state
            .deps
            .protocol
            .room_registry
            .ask(CreateRoom {
                room_jid: grants.scope.room.clone(),
                waddle_id: grants.scope.space_node.clone(),
                channel_id: grants.scope.channel_id.clone(),
                config: waddle_xmpp::muc::RoomConfig {
                    members_only: true,
                    ..Default::default()
                },
            })
            .await
            .expect("room actor");
        let sender: FullJid = format!("{SENDER}/phone").parse().expect("sender");
        let mut fixture = Self {
            grants,
            state,
            authority,
            room,
            sender,
            sender_connection: None,
        };
        for account in ["romeo", "alice"] {
            create_test_session(fixture.state.as_ref(), account).await;
        }
        fixture.legacy_member(SENDER).await;
        fixture.legacy_member(PEER).await;
        let sender = fixture.sender.clone();
        fixture.sender_connection = Some(fixture.seat("romeo", &sender).await);
        Some(fixture)
    }

    /// Give `account` a member affiliation in the room actor: the legacy
    /// store the planner reads its durable audience from.
    async fn legacy_member(&self, account: &str) {
        self.room
            .ask(ChangeAffiliation {
                jid: bare(account),
                affiliation: waddle_xmpp::Affiliation::Member,
            })
            .await
            .expect("legacy affiliation");
    }

    /// Seat `resource` in the room and bind it as a live connection.
    async fn seat(&self, nick: &str, resource: &FullJid) -> Receiver<OutboundStanza> {
        self.room
            .ask(Join {
                session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
                nick: nick.to_owned(),
                real_jid: resource.clone(),
                role: waddle_xmpp::Role::Participant,
                affiliation: waddle_xmpp::Affiliation::Member,
            })
            .await
            .expect("join");
        bind_live_resource(self.state.as_ref(), resource, 16).await
    }

    /// Plan a groupchat from the sender with the production planner.
    async fn plan(&self, origin: &str, body: &str) -> IngressSubmission {
        let room = &self.grants.scope.room;
        let mut submission = self.grants.ingress.submission(Some(origin), body);
        let mut message = Message::new(Some(room.clone().into()));
        message.from = Some(self.sender.clone().into());
        message.type_ = MessageType::Groupchat;
        message.id = Some(xmpp_parsers::message::Id(format!("id-{origin}")));
        message.bodies.insert(Lang::new(), body.to_owned());
        waddle_xmpp_core::xep0359::add_origin_id(&mut message, origin);
        submission.target = NormalizedTarget::Bare(room.clone());
        submission.digest_input = DigestInput::from_parsed(
            &message,
            &DigestContext {
                target: submission.target.clone(),
                server_authorities: vec![self.sender.to_bare(), room.clone()],
                stanza_lang: None,
            },
        )
        .expect("digest");
        let mut dispatcher = waddle_xmpp::protocol::StanzaDispatcher::new();
        waddle_xmpp::protocol::handlers::register_default_message_handlers(&mut dispatcher);
        let mut machine = waddle_xmpp::protocol::XmppStateMachine::new("example.com", dispatcher);
        machine.transition_to_ready(self.sender.clone(), false);
        submission.plan = waddle_server::server::plan_message_dispatch(
            &mut machine,
            message,
            &self.state.recovery_deps(),
        )
        .await;
        assert!(
            submission.plan.rejection.is_none() && submission.plan.failure.is_none(),
            "the room planner refused the groupchat: {:?} {:?}",
            submission.plan.rejection,
            submission.plan.failure
        );
        submission
    }

    /// Commit a planned submission through the production commit path.
    async fn commit(&self, submission: &IngressSubmission) -> IngressDecision {
        commit_submission(&self.grants.ingress.uow, submission, 3)
            .await
            .expect("committed decision")
    }

    /// Run the decision's effects as the server does after its commit.
    async fn execute(&self, decision: &IngressDecision) {
        execute_effects(
            &self.grants.ingress.uow,
            &self.grants.ingress.db,
            decision,
            &ImmediateSink,
            &self.state.recovery_deps(),
            Duration::from_secs(5),
        )
        .await;
    }

    /// The rows of `table` whose `column` holds `account`.
    async fn rows_for(&self, table: &str, column: &str, account: &str) -> i64 {
        self.grants
            .ingress
            .count(&format!("{table} WHERE {column} = '{account}'"))
            .await
    }

    /// What every store an acceptance or its effects write holds for
    /// `account` in this room: inbox rows, notification recovery items,
    /// notification candidates, notification jobs, notification activity
    /// and pending deliveries.
    async fn stored_for(&self, account: &str) -> [i64; 6] {
        [
            self.rows_for("inbox_entries", "user_jid", account).await,
            self.rows_for(
                "groupchat_notification_recovery",
                "recipient_bare_jid",
                account,
            )
            .await,
            self.rows_for("notification_candidates", "recipient_bare_jid", account)
                .await,
            self.rows_for("notification_outbox", "recipient_bare_jid", account)
                .await,
            self.rows_for("notification_activity", "owner_bare_jid", account)
                .await,
            self.rows_for("pending_delivery", "recipient_jid", account)
                .await,
        ]
    }

    async fn close(self) {
        assert!(self.authority.drain_and_join(Duration::from_secs(5)).await);
        drop(self.state);
        self.grants.close().await;
    }
}

/// The accounts the intents name as a recipient of anything: an occupant or
/// a direct route, an inbox row, a notification or a recovery item.
fn planned_accounts(intents: &[IngressEffectIntent]) -> Vec<BareJid> {
    let mut accounts = Vec::new();
    for intent in intents {
        match intent {
            IngressEffectIntent::RouteMucGroupchat { occupants, .. } => {
                accounts.extend(occupants.iter().map(|occupant| occupant.to_bare()));
            }
            IngressEffectIntent::RouteDirect {
                recipient, fanout, ..
            } => {
                accounts.push(recipient.clone());
                accounts.extend(fanout.iter().map(|resource| resource.to_bare()));
            }
            IngressEffectIntent::InboxProject { owner, .. }
            | IngressEffectIntent::NotificationActivityPreview { owner, .. } => {
                accounts.push(owner.clone());
            }
            IngressEffectIntent::GroupchatNotificationRecovery { mutation } => {
                accounts.push(mutation.recipient.clone());
            }
            _ => {}
        }
    }
    accounts.sort();
    accounts.dedup();
    accounts
}

/// Whether the planner planned an inbox row for `account`.
fn plans_inbox_for(intents: &[IngressEffectIntent], account: &str) -> bool {
    planned_work_for(intents, account).contains(&"inbox row")
}

/// The kinds of work the intents plan for `account`: an inbox row, the push
/// of that row to the account's resources, a notification candidate, and
/// the recovery item that keeps the notification from being lost.
fn planned_work_for(intents: &[IngressEffectIntent], account: &str) -> Vec<&'static str> {
    let mut work = Vec::new();
    for intent in intents {
        let (kind, named) = match intent {
            IngressEffectIntent::InboxProject { owner, .. } => ("inbox row", owner),
            IngressEffectIntent::RouteDirect { recipient, .. } => ("inbox push", recipient),
            IngressEffectIntent::NotificationActivityPreview { owner, .. } => {
                ("notification", owner)
            }
            IngressEffectIntent::GroupchatNotificationRecovery { mutation } => {
                ("notification recovery", &mutation.recipient)
            }
            _ => continue,
        };
        if named.as_str() == account {
            work.push(kind);
        }
    }
    work.sort_unstable();
    work.dedup();
    work
}

/// Nothing an accepted decision recorded or returned for execution names
/// `account`: no sealed child, no recorded intent and no external effect.
async fn assert_accepted_without(
    fixture: &ManagedRoomFixture,
    decision: &IngressDecision,
    account: &str,
) {
    assert_eq!(decision.class, IngressDecisionClass::Accepted);
    let manifest = fixture
        .grants
        .manifest(decision.message_key.expect("key"))
        .await
        .expect("sealed manifest");
    assert!(
        !planned_accounts(manifest.recorded_intents()).contains(&bare(account)),
        "a recorded intent names {account}"
    );
    for child in &manifest.children {
        assert_ne!(
            child.target.as_ref().map(|target| target.to_bare()),
            Some(bare(account)),
            "a sealed child targets {account}"
        );
    }
    // The catch-all: whatever shape an intent or an effect has, it cannot
    // carry the account without spelling it.
    let recorded = format!("{:?}", manifest.recorded_intents());
    assert!(!recorded.contains(account), "{recorded}");
    let external = format!("{:?}", decision.external);
    assert!(!external.contains(account), "{external}");
}

/// The groupchat messages a connection received, by body.
fn received_bodies(receiver: &mut Receiver<OutboundStanza>) -> Vec<String> {
    let mut bodies = Vec::new();
    while let Ok(outbound) = receiver.try_recv() {
        let Stanza::Message(message) = outbound.stanza else {
            continue;
        };
        if message.type_ != MessageType::Groupchat {
            continue;
        }
        bodies.extend(message.bodies.values().cloned());
    }
    bodies
}

#[tokio::test]
async fn a_revoked_offline_member_gets_no_inbox_row_push_or_recovery_item() {
    let Some(fixture) = ManagedRoomFixture::start("hg_audience_revoked").await else {
        return;
    };
    // The account was a member, in the canonical store and in the room
    // actor, and its grant is revoked. The legacy affiliation stays, as it
    // does when the legacy write routes are closed for an enrolled scope.
    fixture.legacy_member(REVOKED).await;
    fixture
        .grants
        .set(OWNER, REVOKED, RoomAffiliation::Member)
        .await
        .expect("grant");
    assert_eq!(
        fixture.grants.activate_current(&fixture.grants.scope).await,
        Activation::Activated
    );
    fixture
        .grants
        .set(OWNER, REVOKED, RoomAffiliation::None)
        .await
        .expect("revoke");
    let mut peer = fixture
        .seat("alice", &PEER_RESOURCE.parse().expect("peer"))
        .await;

    // The planner plans for the revoked account as for any offline member:
    // an inbox row with the message preview, its push, a notification and
    // the recovery item of that notification.
    let submission = fixture.plan("revoked-offline", "after the revoke").await;
    let everything = vec![
        "inbox push",
        "inbox row",
        "notification",
        "notification recovery",
    ];
    assert_eq!(
        planned_work_for(&submission.plan.intents, REVOKED),
        everything,
        "the room planner did not plan for the legacy member: {:?}",
        submission.plan.intents
    );
    assert_eq!(planned_work_for(&submission.plan.intents, PEER), everything);

    // The acceptance records and returns nothing for it.
    let decision = fixture.commit(&submission).await;
    assert_accepted_without(&fixture, &decision, REVOKED).await;
    fixture.execute(&decision).await;
    assert_eq!(fixture.stored_for(REVOKED).await, [0; 6]);

    // The admitted member has its inbox row and its copy of the message.
    assert!(fixture.rows_for("inbox_entries", "user_jid", PEER).await > 0);
    assert_eq!(received_bodies(&mut peer), vec!["after the revoke"]);

    // A retransmission replays the acceptance as recorded.
    let replay = fixture.commit(&submission).await;
    assert_eq!(replay.message_key, decision.message_key);
    assert!(!format!("{:?}", replay.external).contains(REVOKED));
    assert_eq!(fixture.stored_for(REVOKED).await, [0; 6]);
    fixture.close().await;
}

#[tokio::test]
async fn a_space_member_without_a_room_grant_gets_nothing() {
    let Some(fixture) = ManagedRoomFixture::start("hg_audience_space").await else {
        return;
    };
    // The legacy permission store names the account a member of the room's
    // parent Space, and the room actor hydrates its durable audience from
    // that store through the production membership source. The account was
    // never granted anything in the canonical store.
    let permissions = fixture.state.deps.app_state.permission_actor.clone();
    permissions
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(ObjectType::Space, fixture.grants.scope.space_node.as_str()),
                Relation::new("member"),
                Subject::user(SPACE_MEMBER),
            ),
        })
        .await
        .expect("legacy Space membership");
    fixture
        .room
        .ask(HydrateDurableRecipients {
            source: Arc::new(PermissionDurableMembershipSource::new(permissions)),
        })
        .await
        .expect("durable membership hydration");
    assert_eq!(fixture.grants.affiliations(SPACE_MEMBER).await, None);

    let submission = fixture
        .plan("space-member", "members of the room only")
        .await;
    assert!(
        plans_inbox_for(&submission.plan.intents, SPACE_MEMBER),
        "the room planner did not plan for the Space member: {:?}",
        submission.plan.intents
    );

    let decision = fixture.commit(&submission).await;
    assert_accepted_without(&fixture, &decision, SPACE_MEMBER).await;
    fixture.execute(&decision).await;
    assert_eq!(fixture.stored_for(SPACE_MEMBER).await, [0; 6]);

    // Nothing is inherited from the parent Space, but the room's own
    // members are served: the offline peer has its inbox row.
    assert!(fixture.rows_for("inbox_entries", "user_jid", PEER).await > 0);
    fixture.close().await;
}

#[tokio::test]
async fn a_stale_online_occupant_gets_no_copy_and_does_not_block_the_members() {
    let Some(fixture) = ManagedRoomFixture::start("hg_audience_stale").await else {
        return;
    };
    // The room actor seats an account, with a live connection, that holds
    // no grant: its occupancy has not caught up with the canonical state.
    fixture.legacy_member(STALE).await;
    let mut stale = fixture
        .seat("erin", &STALE_RESOURCE.parse().expect("stale occupant"))
        .await;
    let mut peer = fixture
        .seat("alice", &PEER_RESOURCE.parse().expect("peer"))
        .await;
    assert_eq!(fixture.grants.affiliations(STALE).await, None);

    for round in 0..3 {
        let body = format!("delivered to members {round}");
        let submission = fixture.plan(&format!("stale-online-{round}"), &body).await;
        // The planner routes the message to the seated account.
        let routed = submission.plan.intents.iter().any(|intent| {
            matches!(
                intent,
                IngressEffectIntent::RouteMucGroupchat { occupants, .. }
                    if occupants.iter().any(|occupant| occupant.to_bare() == bare(STALE))
            )
        });
        assert!(
            routed,
            "the room planner did not route to the seated occupant: {:?}",
            submission.plan.intents
        );

        // The stanza is accepted, for the members: the seated account
        // without a grant is left out and keeps nobody else from the
        // message, stanza after stanza.
        let decision = fixture.commit(&submission).await;
        assert_accepted_without(&fixture, &decision, STALE).await;
        fixture.execute(&decision).await;
        assert_eq!(received_bodies(&mut peer), vec![body]);
        assert!(received_bodies(&mut stale).is_empty());
        assert_eq!(fixture.stored_for(STALE).await, [0; 6]);
    }
    fixture.close().await;
}

/// How a test rewrites one principal nested in the planned work for an
/// admitted account, leaving the principal the work itself names alone.
#[derive(Clone, Copy, Debug)]
enum NestedPrincipal {
    /// The recovery item an inbox row carries.
    InboxRecovery,
    /// The recipient of a notification candidate.
    CandidateRecipient,
    /// The recovery item a notification candidate carries.
    CandidateRecovery,
    /// The route an inbox push is recorded under.
    PushReceipt,
}

impl NestedPrincipal {
    /// Name `other` in place of `owner` in the first planned effect of
    /// `owner` that carries this nested principal. The answer is whether
    /// such an effect was found.
    fn rewrite(self, plan: &mut [PlannedEffect], owner: &BareJid, other: &BareJid) -> bool {
        plan.iter_mut()
            .any(|planned| self.rewrite_effect(&mut planned.effect, owner, other))
    }

    fn rewrite_effect(self, effect: &mut Effect, owner: &BareJid, other: &BareJid) -> bool {
        match (self, effect) {
            (
                Self::InboxRecovery,
                Effect::Durable(DurableEffect::Room(DurableRoomEffect::ProjectGroupchatInbox {
                    owner: named,
                    recovery: Some(recovery),
                    ..
                })),
            ) if named == owner => {
                recovery.key.recipient = other.clone();
                true
            }
            (
                Self::CandidateRecipient,
                Effect::External(ExternalEffect::Room(ExternalRoomEffect::NotificationCandidate {
                    owner: named,
                    candidate: Some(candidate),
                    ..
                })),
            ) if named == owner => {
                **candidate = NotificationCandidate::groupchat(
                    other.clone(),
                    candidate.conversation_jid().clone(),
                    candidate.sender_jid().clone(),
                    candidate.thread_id().clone(),
                    candidate.archive_stanza_id().clone(),
                    candidate.class(),
                )
                .expect("candidate for another recipient");
                true
            }
            (
                Self::CandidateRecovery,
                Effect::External(ExternalEffect::Room(ExternalRoomEffect::NotificationCandidate {
                    owner: named,
                    recovery: Some(recovery),
                    ..
                })),
            ) if named == owner => {
                recovery.key.recipient = other.clone();
                true
            }
            (
                Self::PushReceipt,
                Effect::External(ExternalEffect::Direct(ExternalDirectEffect::PushInboxUpdate {
                    owner: named,
                    receipt: Some(receipt),
                    ..
                })),
            ) if named == owner => match receipt.as_mut() {
                IngressEffectIntent::RouteDirect { recipient, .. } => {
                    *recipient = other.clone();
                    true
                }
                _ => false,
            },
            _ => false,
        }
    }
}

#[tokio::test]
async fn work_for_a_member_that_nests_another_principal_refuses_the_stanza() {
    let Some(fixture) = ManagedRoomFixture::start("hg_audience_nested").await else {
        return;
    };
    // The planner plans for the offline peer, an admitted member, an inbox
    // row with its recovery item, the push of that row, and a notification
    // candidate with its own recovery item. Each kind of work is kept or
    // dropped by the principal it names, so a principal nested in it that
    // is not that principal would be carried past the audience. Such work
    // is not one a managed room accounts for: the stanza is refused, even
    // when the nested principal is itself admitted.
    let peer = bare(PEER);
    for (round, other) in [bare(REVOKED), bare(SENDER)].iter().enumerate() {
        for nested in [
            NestedPrincipal::InboxRecovery,
            NestedPrincipal::CandidateRecipient,
            NestedPrincipal::CandidateRecovery,
            NestedPrincipal::PushReceipt,
        ] {
            let mut submission = fixture
                .plan(&format!("nested-{nested:?}-{round}"), "for the peer only")
                .await;
            assert!(
                nested.rewrite(&mut submission.plan.plan, &peer, other),
                "the room planner planned no {nested:?} for the peer: {:?}",
                submission.plan.plan
            );
            let decision = fixture.commit(&submission).await;
            assert_eq!(
                decision.class,
                IngressDecisionClass::PolicyDenied,
                "{nested:?} naming {other}"
            );
            assert!(fixture
                .grants
                .manifest(decision.message_key.expect("key"))
                .await
                .is_none());
        }
    }
    assert_eq!(fixture.stored_for(PEER).await, [0; 6]);
    assert_eq!(fixture.stored_for(REVOKED).await, [0; 6]);

    // The same plan as the planner produced it is accepted for the peer.
    let submission = fixture.plan("nested-unchanged", "for the peer").await;
    let decision = fixture.commit(&submission).await;
    assert_eq!(decision.class, IngressDecisionClass::Accepted);
    fixture.execute(&decision).await;
    assert!(fixture.rows_for("inbox_entries", "user_jid", PEER).await > 0);
    fixture.close().await;
}

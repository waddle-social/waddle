//! Call policy, receipt-based expiry, and class-separated coalescing.

use crate::support::{
    bare, candidate, drain_deps_with_noop_activity, enqueue_jobs_for_test, noop_activity_reader,
    register_push_target, settings_projection, store, target, StubRoomPolicy,
};
use jid::{BareJid, Jid};
use waddle_server::notification_outbox::{
    DndReader, DndState, DrainBatch, DrainScope, NotificationCall, NotificationCandidate,
    NotificationClass, NotificationMessageHints, NotificationOutboxError, WADDLE_PUSH_CONTEXT_NS,
};
use waddle_server::notification_settings_projection::{
    ConversationKind, NotificationSettingsProjection, NotificationSettingsSource,
};
use waddle_xmpp::push::InMemoryPushStore;
use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;
use waddle_xmpp::xep::NotificationLevel;
use waddle_xmpp_core::xep0359::StanzaId;
use xmpp_parsers::jingle::SessionId;

pub(super) fn call_candidate(item: &str, sid: &str, video: bool) -> NotificationCandidate {
    let recipient = bare("alice@example.com");
    NotificationCandidate::call_with_hints(
        recipient.clone(),
        "bob@example.com/phone".parse().expect("sender"),
        StanzaId::new(item, Jid::from(recipient)),
        NotificationCall {
            session_id: SessionId(sid.into()),
            video,
        },
        NotificationMessageHints::none(),
    )
    .expect("call candidate")
}

pub(super) fn missed_call_candidate(item: &str, sid: &str) -> NotificationCandidate {
    let recipient = bare("alice@example.com");
    NotificationCandidate::missed_call_with_hints(
        recipient.clone(),
        "bob@example.com/laptop".parse().expect("sender"),
        StanzaId::new(item, Jid::from(recipient)),
        SessionId(sid.into()),
        NotificationMessageHints::none(),
    )
    .expect("missed call candidate")
}

#[tokio::test]
async fn latest_call_replaces_context_without_merging_with_dm_and_uses_receipt_expiry() {
    let store = store().await;
    let target = target();
    enqueue_jobs_for_test(
        &store,
        &call_candidate("first-call", "first-session", false),
        std::slice::from_ref(&target),
    )
    .await;
    let second = call_candidate("second-call", "second-session", true);
    store
        .insert_candidate(&second)
        .await
        .expect("insert second call");
    store
        .execute(
            "UPDATE notification_candidates SET created_at_ms = 1700000000123 WHERE stanza_id = ?",
            waddle_server::db_params!["second-call"],
        )
        .await
        .expect("set receipt time");
    enqueue_jobs_for_test(&store, &second, std::slice::from_ref(&target)).await;
    enqueue_jobs_for_test(&store, &candidate("plain-dm"), &[target]).await;
    let jobs = store.pending_outbox_jobs().await.expect("jobs");
    assert_eq!(
        jobs.len(),
        2,
        "call and DM have distinct coalescing classes"
    );
    let call = jobs
        .iter()
        .find(|job| job.class() == NotificationClass::Call)
        .expect("call job");
    assert_eq!(call.message_count(), 2);
    let context = call.context();
    let invitation = context
        .get_child("call", WADDLE_PUSH_CONTEXT_NS)
        .expect("call context");
    assert_eq!(context.attr("stanza-id"), Some("second-call"));
    assert_eq!(invitation.attr("sid"), Some("second-session"));
    assert_eq!(invitation.attr("video"), Some("true"));
    assert_eq!(invitation.attr("expires"), Some("1700000045"));
}

struct FixedDnd(bool);
#[async_trait::async_trait]
impl DndReader for FixedDnd {
    async fn dnd_state(&self, _: &BareJid) -> Result<DndState, NotificationOutboxError> {
        Ok(if self.0 {
            DndState::Active
        } else {
            DndState::Inactive
        })
    }
}

#[tokio::test]
async fn calls_and_missed_calls_respect_never_dnd_blocking_and_pass_on_mention() {
    for missed in [false, true] {
        for (mode, dnd, blocked, expected) in [
            (
                NotificationLevel::Never,
                false,
                false,
                Some("xep0492_never"),
            ),
            (NotificationLevel::OnMention, false, false, None),
            (NotificationLevel::Always, true, false, Some("waddle_dnd")),
            (
                NotificationLevel::Always,
                false,
                true,
                Some("xep0191_blocked"),
            ),
        ] {
            let store = store().await;
            let call = if missed {
                missed_call_candidate("missed-call-policy", "session")
            } else {
                call_candidate("call-policy", "session", false)
            };
            let settings = settings_projection().await;
            settings
                .upsert(&NotificationSettingsProjection {
                    owner_bare_jid: call.recipient_bare_jid().clone(),
                    conversation_jid: call.conversation_jid().clone(),
                    conversation_kind: ConversationKind::Direct,
                    mode,
                    source: NotificationSettingsSource::Xep0402Bookmarks,
                    source_item_jid: call.conversation_jid().clone(),
                    updated_at_ms: 1,
                    rich_payload_opt_in: false,
                    source_version: 1,
                })
                .await
                .expect("notification level");
            let push = InMemoryPushStore::new();
            register_push_target(&push, call.recipient_bare_jid(), &target()).await;
            let blocking = InMemoryBlockingStorage::new();
            if blocked {
                blocking.set_blocklist_jids(
                    call.recipient_bare_jid().clone(),
                    vec![Jid::from(call.conversation_jid().clone())],
                );
            }
            store.insert_candidate(&call).await.expect("insert call");
            let room_policy = StubRoomPolicy::new();
            store
                .drain_pending_candidates_into_outbox(
                    &push,
                    &blocking,
                    &settings,
                    drain_deps_with_noop_activity(
                        &room_policy,
                        &FixedDnd(dnd),
                        noop_activity_reader(),
                    ),
                    &bare("push.example.com"),
                    DrainBatch {
                        scope: DrainScope::All,
                        batch_size: 16,
                    },
                )
                .await
                .expect("drain");
            assert_eq!(
                store.pending_outbox_jobs().await.expect("jobs").len(),
                usize::from(expected.is_none())
            );
            let mut rows = store
                .query("SELECT suppressed_reason FROM notification_candidates", ())
                .await
                .expect("audit");
            let audit = rows
                .next()
                .await
                .expect("row")
                .expect("candidate")
                .get::<Option<String>>(0)
                .expect("reason");
            assert_eq!(audit.as_deref(), expected);
        }
    }
}

#[cfg(feature = "test-support")]
#[test]
fn direct_intrinsics_classify_only_bodyless_audio_or_video_propose_as_call() {
    use waddle_server::test_support::direct_notification_candidate_from_envelope_for_test as direct_candidate_from_envelope;
    use xmpp_parsers::message::Message;

    use waddle_xmpp::xep::xep0353::{build_proceed, build_propose, CallOffer};
    use waddle_xmpp_core::xep0359::StanzaId;
    use xmpp_parsers::jingle::SessionId;
    let recipient = bare("alice@example.com");
    let sender: Jid = "bob@example.com/phone".parse().expect("sender");
    let archive = StanzaId::new("archive", Jid::from(recipient.clone()));
    for (offer, video) in [
        (CallOffer::audio_only(), false),
        (CallOffer::audio_video(), true),
    ] {
        let mut message = Message::new(None::<Jid>);
        message
            .payloads
            .push(build_propose(SessionId("session".into()), offer));
        let call =
            direct_candidate_from_envelope(&message, &recipient, &sender, &archive).expect("call");
        assert_eq!(call.class(), NotificationClass::Call);
        assert_eq!(call.call().expect("call data").video, video);
        message
            .bodies
            .insert("no".into(), "body in another language".into());
        assert_eq!(
            direct_candidate_from_envelope(&message, &recipient, &sender, &archive)
                .expect("message")
                .class(),
            NotificationClass::DirectMessage
        );
    }
    let mut without_media = Message::new(None::<Jid>);
    without_media.payloads.push(build_propose(
        SessionId("no-media".into()),
        CallOffer {
            audio: false,
            video: false,
        },
    ));
    assert_eq!(
        direct_candidate_from_envelope(&without_media, &recipient, &sender, &archive)
            .expect("proposal without audio or video")
            .class(),
        NotificationClass::DirectMessage
    );
    for payload in [
        build_propose(SessionId("".into()), CallOffer::audio_only()),
        build_proceed(SessionId("session".into())).into(),
    ] {
        let mut message = Message::new(None::<Jid>);
        message.payloads.push(payload);
        assert_eq!(
            direct_candidate_from_envelope(&message, &recipient, &sender, &archive)
                .expect("message")
                .class(),
            NotificationClass::DirectMessage
        );
    }
}

#[cfg(feature = "test-support")]
#[tokio::test]
async fn offline_call_signals_preserve_pending_delivery_without_generic_alerts() {
    use minidom::Element;
    use std::sync::Arc;
    use std::time::Duration;
    use waddle_server::db::{DatabaseConfig, DatabasePool, PoolConfig};
    use waddle_server::ingress::commit::commit_submission;
    use waddle_server::ingress::execute::execute_effects;
    use waddle_server::ingress::{
        plan_message_dispatch, ImmediateSink, IngressDecisionClass, RecoveryEnvironment,
    };
    use waddle_server::test_support::{seed_local_account, websocket_state_with_ingress};
    use waddle_xmpp::ingress::{
        DigestContext, DigestInput, IngressEffectIntent, NotificationActivityMutation,
    };
    use waddle_xmpp::protocol::{StanzaDispatcher, XmppStateMachine};
    use waddle_xmpp::xep::xep0334::{add_hint, Hint};
    use waddle_xmpp::xep::xep0353::{build_propose, build_retract, CallOffer, NS_JINGLE_MESSAGE};

    let fixture = crate::ingress_support::IngressFixture::sqlite().await;
    let pool = Arc::new(
        DatabasePool::try_new(
            DatabaseConfig::new(fixture.db.driver(), fixture.db.database_url()),
            PoolConfig,
        )
        .await
        .expect("shared database pool"),
    );
    let state = websocket_state_with_ingress(pool, Arc::new(fixture.authority().await)).await;
    seed_local_account(&state, "romeo").await;
    seed_local_account(&state, "juliet").await;
    assert_eq!(
        fixture
            .count("sessions WHERE user_jid = 'juliet@example.com'")
            .await,
        0,
        "recipient account has no session"
    );
    let deps = state.recovery_deps();
    let sid = SessionId("offline-session".into());
    for (origin, payload, body, expected_candidates) in [
        (
            "offline-retract",
            build_retract(sid.clone()).into(),
            false,
            0,
        ),
        (
            "offline-audio-propose",
            build_propose(sid.clone(), CallOffer::audio_only()),
            false,
            1,
        ),
        (
            "offline-missed-call",
            build_retract(sid.clone()).into(),
            false,
            2,
        ),
        (
            "offline-missed-call-duplicate",
            build_retract(sid.clone()).into(),
            false,
            2,
        ),
        (
            "offline-retract-with-body",
            build_retract(sid).into(),
            true,
            3,
        ),
        (
            "offline-future-signal",
            Element::builder("future-signal", NS_JINGLE_MESSAGE).build(),
            false,
            4,
        ),
    ] {
        let mut submission = fixture.submission(Some(origin), "call ended");
        let mut message = submission.plan.sanitized_message.clone();
        if !body {
            message.bodies.clear();
        }
        message.payloads.push(payload);
        add_hint(&mut message, Hint::Store);
        submission.digest_input = DigestInput::from_parsed(
            &message,
            &DigestContext {
                target: submission.target.clone(),
                server_authorities: vec![fixture.principal.bare_jid().clone()],
                stanza_lang: None,
            },
        )
        .expect("digest");
        let mut dispatcher = StanzaDispatcher::new();
        waddle_xmpp::protocol::handlers::register_default_message_handlers(&mut dispatcher);
        let mut machine = XmppStateMachine::new("example.com", dispatcher);
        machine.transition_to_ready(submission.sender.clone(), false);
        submission.plan = plan_message_dispatch(&mut machine, message, &deps).await;
        assert!(
            submission.plan.failure.is_none(),
            "{origin}: {:?}",
            submission.plan.failure
        );
        assert!(
            submission.plan.rejection.is_none(),
            "{origin}: {:?}",
            submission.plan.rejection
        );
        let notification_intents = submission
            .plan
            .intents
            .iter()
            .filter(|intent| {
                matches!(
                    intent,
                    IngressEffectIntent::NotificationActivityPreview {
                        mutation: NotificationActivityMutation::NotificationCandidate { .. },
                        ..
                    }
                )
            })
            .count();
        assert_eq!(
            notification_intents,
            usize::from(expected_candidates > 0),
            "{origin}"
        );
        assert!(
            submission.plan.intents.iter().any(|intent| matches!(
                intent,
                IngressEffectIntent::NotificationActivityPreview {
                    mutation: NotificationActivityMutation::OfflineDelivery { .. },
                    ..
                }
            )),
            "{origin}: offline delivery marker retained"
        );
        let decision = commit_submission(&fixture.uow, &submission, 1)
            .await
            .expect("commit");
        assert_eq!(decision.class, IngressDecisionClass::Accepted);
        execute_effects(
            &fixture.uow,
            &fixture.db,
            &decision,
            &ImmediateSink,
            &deps,
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(
            fixture.count("notification_candidates").await,
            expected_candidates,
            "{origin}"
        );
    }
    assert_eq!(fixture.count("pending_delivery").await, 6);
    assert_eq!(
        fixture.count("mam_messages").await,
        12,
        "both personal archives retained"
    );
    assert_eq!(
        fixture
            .count("notification_candidates WHERE class = 'call'")
            .await,
        1
    );
    assert_eq!(
        fixture
            .count("notification_candidates WHERE class = 'dm'")
            .await,
        2
    );
    assert_eq!(
        fixture
            .count("notification_candidates WHERE class = 'missed_call'")
            .await,
        1
    );
    drop(state);
    fixture.close().await;
}

#[tokio::test]
async fn missed_call_uses_normal_drain_and_never_merges_with_dm_or_invitation() {
    let store = store().await;
    let target = target();
    for candidate in [
        missed_call_candidate("missed-archive", "session"),
        candidate("dm-archive"),
        call_candidate("call-archive", "session", false),
    ] {
        store.insert_candidate(&candidate).await.expect("candidate");
    }
    let push = InMemoryPushStore::new();
    register_push_target(&push, &bare("alice@example.com"), &target).await;
    let settings = settings_projection().await;
    let blocking = InMemoryBlockingStorage::new();
    let room_policy = StubRoomPolicy::new();
    store
        .drain_pending_candidates_into_outbox(
            &push,
            &blocking,
            &settings,
            drain_deps_with_noop_activity(&room_policy, &FixedDnd(false), noop_activity_reader()),
            &bare("push.example.com"),
            DrainBatch {
                scope: DrainScope::CallsOnly,
                batch_size: 16,
            },
        )
        .await
        .expect("call drain");
    let pending = store
        .pending_candidates(16)
        .await
        .expect("normal candidates remain");
    assert_eq!(pending.len(), 2);
    assert!(pending
        .iter()
        .any(|candidate| candidate.class() == NotificationClass::MissedCall));
    assert_eq!(
        store
            .pending_outbox_jobs()
            .await
            .expect("only invitation outboxed")
            .len(),
        1
    );
    store
        .drain_pending_candidates_into_outbox(
            &push,
            &blocking,
            &settings,
            drain_deps_with_noop_activity(&room_policy, &FixedDnd(false), noop_activity_reader()),
            &bare("push.example.com"),
            DrainBatch {
                scope: DrainScope::All,
                batch_size: 16,
            },
        )
        .await
        .expect("normal drain");
    let jobs = store.pending_outbox_jobs().await.expect("jobs");
    assert_eq!(jobs.len(), 3);
    let missed = jobs
        .iter()
        .find(|job| job.class() == NotificationClass::MissedCall)
        .expect("missed job");
    assert!(missed
        .context()
        .get_child("call", WADDLE_PUSH_CONTEXT_NS)
        .is_none());
    let calls = store
        .claim_due_outbox_jobs(DrainScope::CallsOnly, 16)
        .await
        .expect("call claims");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].class(), NotificationClass::Call);
    let normal = store
        .claim_due_outbox_jobs(DrainScope::All, 16)
        .await
        .expect("normal claims");
    assert_eq!(normal.len(), 2);
    assert!(normal
        .iter()
        .any(|job| job.class() == NotificationClass::MissedCall));
}

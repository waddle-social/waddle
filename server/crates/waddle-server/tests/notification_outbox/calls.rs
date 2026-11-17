//! Call policy, receipt-based expiry, and class-separated coalescing.

use crate::support::{
    bare, candidate, drain_deps_with_noop_activity, enqueue_jobs_for_test, noop_activity_reader,
    register_push_target, settings_projection, store, target, StubRoomPolicy,
};
use jid::{BareJid, Jid};
use waddle_server::notification_outbox::{
    DndReader, DndState, NotificationCall, NotificationCandidate, NotificationClass,
    NotificationMessageHints, NotificationOutboxError, WADDLE_PUSH_CONTEXT_NS,
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
async fn calls_respect_never_dnd_blocking_and_pass_on_mention() {
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
        let call = call_candidate("call-policy", "session", false);
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
                drain_deps_with_noop_activity(&room_policy, &FixedDnd(dnd), noop_activity_reader()),
                &bare("push.example.com"),
                16,
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

#[cfg(feature = "test-support")]
#[test]
fn direct_intrinsics_classify_only_bodyless_audio_or_video_propose_as_call() {
    use waddle_server::test_support::direct_notification_candidate_from_envelope_for_test as direct_candidate_from_envelope;
    use xmpp_parsers::message::Message;

    use waddle_xmpp::xep::xep0353::{build_proceed, build_propose, build_retract, CallOffer};
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
        build_retract(SessionId("session".into())).into(),
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

use super::super::effects::{EffectSink, PlanFailure, PlanSink};
use super::*;
use crate::ingress::{
    commit::commit_submission, test_support::IngressFixture, IngressDecisionClass,
    IngressStreamIdentity,
};
use crate::ingress_uow::SmIngressStreamRepository;
use waddle_xmpp::{
    ingress::WireHandledCount,
    mam::{MamStorage, SqlxMamStorage},
    pending_delivery::SmSessionId,
};

async fn correction_second_read_failure(fixture: IngressFixture) {
    let mam: Arc<dyn MamStorage> = Arc::new(
        SqlxMamStorage::open(fixture.db.database_url())
            .await
            .expect("MAM"),
    );
    let socket = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let registry = ConnectionRegistry::new();
    let mut deps = Deps::registry_only(&registry);
    deps.mam_storage = Some(&mam);
    deps.web_socket_state = Some(&socket);
    let mut submission = fixture.submission(Some("correction-second-read"), "updated preview");
    let sender = submission.sender.to_bare();
    let mut original = waddle_xmpp::mam::ArchivedMessage::for_test(
        submission.sender.clone().into(),
        "juliet@example.com".parse().expect("recipient"),
    );
    original.id = "original-archive".into();
    original.origin_id = Some(waddle_xmpp_core::xep0359::OriginId::new("original-origin"));
    original.stanza_id = Some(StanzaId::new("original-wire", sender.clone().into()));
    mam.store_message(&sender, &original)
        .await
        .expect("original archive");
    submission.plan.sanitized_message.payloads.push(
        waddle_xmpp::xep::xep0308::build_replace_element("original-origin"),
    );
    waddle_xmpp_core::xep0359::add_stanza_id(
        &mut submission.plan.sanitized_message,
        &StanzaId::new("correction-archive", sender.clone().into()),
    );
    let slot = uuid::Uuid::new_v4();
    let preview_url = url::Url::parse(&socket.deps.auth_state.base_url).expect("base URL").join(&format!("/api/files/{slot}/link-preview-86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16.png")).expect("preview URL");
    waddle_xmpp::xep::add_reference(
        &mut submission.plan.sanitized_message,
        &waddle_xmpp::xep::Reference::data(preview_url.to_string()),
    );
    let incoming = submission.plan.sanitized_message.clone();
    let stream_id = SmSessionId::new("correction-second-read-stream");
    let mut tx = fixture.uow.begin().await.expect("begin");
    let sm_ingress_id = SmIngressStreamRepository::mint(&mut tx, &stream_id)
        .await
        .expect("stream");
    tx.commit().await.expect("stream commit");
    submission.identity = IngressStreamIdentity::Resumable {
        stream_id,
        sm_ingress_id,
        #[cfg(feature = "clustering")]
        owner: waddle_xmpp::ownership::NodeIdentity::new("unused", "single-node"),
        #[cfg(feature = "clustering")]
        claim_epoch: waddle_xmpp::ownership::ClaimEpoch(1),
        reserved_wire_position: WireHandledCount::new(1),
        checkpoint_h: WireHandledCount::new(1),
    };
    // Resolve the original through the validator's actual read before failing
    // the separate archive-side lookup used for preview reference keys.
    let reference = MessageRef::OriginId {
        sender: submission.sender.clone().into(),
        origin_id: original.origin_id.clone().expect("origin"),
    };
    assert!(super::super::archive_lookup::lookup_archived_message(
        &deps,
        &sender,
        waddle_xmpp::mam::MamArchiveKind::Personal,
        &reference
    )
    .await
    .is_some());
    fixture
        .execute(
            "ALTER TABLE mam_messages RENAME TO unavailable_mam_messages",
            (),
        )
        .await;
    for failed in [true, false] {
        let sink = PlanSink::new();
        let capture = crate::ingress::IngressEffectCapture::new();
        sink.observe_message(&incoming);
        let mut planned = super::super::message_plan::build_plan_deps(&deps, &sink);
        planned.ingress_effect_capture = Some(capture.clone());
        archive_direct(
            &planned,
            sender.clone(),
            submission.sender.clone().into(),
            incoming.to.clone().expect("to"),
            Box::new(incoming.clone()),
        )
        .await;
        submission.plan = super::super::message_plan::finish_plan(
            &sink,
            &capture,
            incoming.clone(),
            Some(submission.sender.clone()),
        );
        if failed {
            assert_eq!(submission.plan.failure, Some(PlanFailure::RichTargetLookup));
            assert!(
                submission.plan.intents.iter().any(|intent| matches!(
                    intent,
                    IngressEffectIntent::ArchiveAuthoritative { .. }
                )),
                "the earlier archive step was planned"
            );
            assert!(
                !submission.plan.intents.iter().any(|intent| matches!(
                    intent,
                    IngressEffectIntent::LinkPreviewMediaRef { .. }
                )),
                "no fallback preview key can escape"
            );
            fixture
                .execute(
                    "ALTER TABLE unavailable_mam_messages RENAME TO mam_messages",
                    (),
                )
                .await;
            let error = commit_submission(&fixture.uow, &submission, 1)
                .await
                .expect_err("failed re-resolution must refuse partial plan");
            assert_eq!(error.class(), IngressDecisionClass::Storage);
            assert!(!error.class().advances());
            for table in [
                "ingress_messages",
                "ingress_origin_aliases",
                "ingress_effect_intents",
                "ingress_effect_receipts",
                "ingress_sm_refs",
                "ingress_deliveries",
                "inbox_entries",
            ] {
                assert_eq!(fixture.count(table).await, 0, "{table}");
            }
            assert_eq!(fixture.count("mam_messages").await, 1);
            assert_eq!(
                fixture
                    .count("ingress_sm_streams WHERE handled_ordinal = 0 AND checkpoint_h = 0")
                    .await,
                1
            );
        } else {
            assert_eq!(submission.plan.failure, None);
            assert!(submission.plan.intents.iter().any(|intent| matches!(intent, IngressEffectIntent::LinkPreviewMediaRef { mutation } if mutation.upload_slot_id == slot && mutation.message_id.as_str() == "original-wire")), "healthy preview mutation uses the original wire ID");
            assert_eq!(
                resolve_direct_correction_target_message_id(
                    &deps,
                    &sender,
                    &sender,
                    "original-origin"
                )
                .await
                .expect("healthy read")
                .expect("mapping")
                .as_str(),
                "original-wire"
            );
            let decision = commit_submission(&fixture.uow, &submission, 1)
                .await
                .expect("healthy retry commits");
            assert!(decision.class.advances());
            assert_eq!(fixture.count("ingress_origin_aliases").await, 1);
            assert_eq!(fixture.count("mam_messages").await, 2);
            assert_eq!(
                fixture
                    .count("ingress_sm_streams WHERE handled_ordinal = 1 AND checkpoint_h = 1")
                    .await,
                1
            );
        }
    }
    drop(mam);
    fixture.close().await;
}

#[tokio::test]
async fn correction_second_mam_read_failure_is_nonadvancing_sqlite() {
    correction_second_read_failure(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn correction_second_mam_read_failure_is_nonadvancing_postgres() {
    if let Some(fixture) = IngressFixture::postgres("correction_second_read").await {
        correction_second_read_failure(fixture).await;
    }
}

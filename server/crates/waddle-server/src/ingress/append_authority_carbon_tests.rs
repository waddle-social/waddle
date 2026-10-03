//! Carbon receipt authority includes the direction, original payload and resource.
use super::*;
use crate::{
    ingress::{
        commit::commit_submission, identity::IngressAppendObligationRef,
        test_support::IngressFixture,
    },
    server::routes::interpret::SmIngressAppendContext,
};
use waddle_xmpp::{ingress::IngressEffectIntent, protocol::CarbonKind};
use xmpp_parsers::message::Message;

fn wrapper(
    inner: &Message,
    owner: &jid::BareJid,
    target: &jid::FullJid,
    kind: CarbonKind,
) -> Stanza {
    let message = match kind {
        CarbonKind::Sent => waddle_xmpp_core::carbons::build_sent_carbon(
            inner,
            &owner.to_string(),
            &target.to_string(),
        ),
        CarbonKind::Received => waddle_xmpp_core::carbons::build_received_carbon(
            inner,
            &owner.to_string(),
            &target.to_string(),
        ),
    }
    .expect("carbon wrapper");
    Stanza::Message(message)
}

async fn carbon_append_authority(fixture: IngressFixture) {
    for (origin, kind, remote) in [
        ("local-sent", CarbonKind::Sent, false),
        ("local-received", CarbonKind::Received, false),
        ("remote-sent", CarbonKind::Sent, true),
        ("remote-received", CarbonKind::Received, true),
    ] {
        let mut submission = fixture.submission(Some(origin), "frozen body");
        let owner: jid::BareJid = match kind {
            CarbonKind::Sent => submission.sender.to_bare(),
            CarbonKind::Received => "juliet@example.com".parse().expect("recipient"),
        };
        let excluded = owner.with_resource_str("phone").expect("excluded");
        let target = owner.with_resource_str("laptop").expect("target");
        let intent = if remote {
            IngressEffectIntent::RelayCarbons {
                owner: owner.clone(),
                exclude: vec![excluded.clone()],
                kind,
            }
        } else {
            IngressEffectIntent::Carbons {
                excluded_source: excluded.clone(),
                carbon_recipients: vec![target.clone()],
                kind,
            }
        };
        submission.plan.sanitized_message.thread = Some(xmpp_parsers::message::Thread {
            id: "child-thread".into(),
            parent: None,
        });
        waddle_xmpp_core::parser_utils::reattach_thread_parent(
            &mut submission.plan.sanitized_message,
            "parent-thread".into(),
            waddle_xmpp_core::xep0201::CLIENT_STANZA_NS,
        );
        // Processing can append metadata after the parent-bearing thread;
        // canonical XML parsing moves that thread to the final payload slot.
        waddle_xmpp_core::xep0359::add_stanza_id(
            &mut submission.plan.sanitized_message,
            &waddle_xmpp_core::xep0359::StanzaId::new(
                "sender-stamp",
                submission.sender.to_bare().into(),
            ),
        );
        submission.plan.intents.push(intent.clone());
        let decision = commit_submission(&fixture.uow, &submission, 1)
            .await
            .expect("commit carbon authority");
        let context = SmIngressAppendContext {
            archive_positions: Vec::new(),
            dispatch_stream: None,
            message_key: decision.message_key.expect("canonical"),
            receipt: crate::ingress::receipt_key(&intent).expect("receipt"),
            received_at: None,
        };
        let original = &submission.plan.sanitized_message;
        let carbon = wrapper(original, &owner, &target, kind);
        let reference =
            IngressAppendObligationRef::for_message(Some(&context), &carbon).expect("carbon key");
        assert_eq!(
            reference.sender_bare,
            submission.sender.to_bare(),
            "received carbon uses original sender"
        );
        check_stanza_binding(
            &carbon,
            &reference.sender_bare,
            reference.receipt.kind.to_storage(),
        )
        .expect("typed binding");
        check_canonical_obligation(&fixture.db, &carbon, &reference)
            .await
            .expect("frozen carbon accepted");
        let wrong_direction = match kind {
            CarbonKind::Sent => CarbonKind::Received,
            CarbonKind::Received => CarbonKind::Sent,
        };
        let mut edited = original.clone();
        edited
            .bodies
            .insert(Default::default(), "changed body".into());
        let mut changed_parent = original.clone();
        changed_parent
            .payloads
            .iter_mut()
            .find(|payload| {
                waddle_xmpp_core::xep0201::is_thread_element_for_stanza(
                    payload,
                    waddle_xmpp_core::xep0201::CLIENT_STANZA_NS,
                )
            })
            .expect("parent-bearing thread")
            .set_attr(
                minidom::rxml::Namespace::NONE,
                minidom::rxml::xml_ncname!("parent").to_owned(),
                "forged-parent",
            );
        for invalid in [
            wrapper(original, &owner, &target, wrong_direction),
            wrapper(original, &owner, &excluded, kind),
            wrapper(&edited, &owner, &target, kind),
            wrapper(&changed_parent, &owner, &target, kind),
        ] {
            assert!(matches!(
                check_canonical_obligation(&fixture.db, &invalid, &reference).await,
                Err(AppendAuthorityRejection::CarbonObligationMismatch)
            ));
        }
        if !remote {
            let late = owner.with_resource_str("late").expect("late");
            assert!(check_canonical_obligation(
                &fixture.db,
                &wrapper(original, &owner, &late, kind),
                &reference
            )
            .await
            .is_err());
        }
        assert!(
            check_resource_binding(&carbon, reference.receipt.kind.to_storage(), &excluded)
                .is_err()
        );
        let mut forged = reference.clone();
        forged.receipt.semantic_identity_hash = [0; 32];
        assert!(check_canonical_obligation(&fixture.db, &carbon, &forged)
            .await
            .is_err());
    }
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_carbon_append_requires_exact_frozen_authority() {
    carbon_append_authority(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_carbon_append_requires_exact_frozen_authority() {
    if let Some(fixture) = IngressFixture::postgres("carbon_append_authority").await {
        carbon_append_authority(fixture).await;
    }
}

async fn archive_free_carbon_stamps(fixture: IngressFixture) {
    use crate::server::routes::interpret::FullJidDeliveryOutcome;
    use waddle_xmpp::{
        registry::BroadcastOutcome,
        xep::xep0353::{build_propose, CallOffer},
    };
    use waddle_xmpp_core::xep0359::{add_stanza_id, build_stanza_id_element, StanzaId};
    let authority = fixture.authority().await;
    for kind in [CarbonKind::Sent, CarbonKind::Received] {
        let mut submission = fixture.submission(None, "");
        let sender = submission.sender.to_bare();
        let owner: jid::BareJid = match kind {
            CarbonKind::Sent => sender.clone(),
            CarbonKind::Received => "juliet@example.com".parse().expect("recipient"),
        };
        let target = owner.with_resource_str("laptop").expect("target");
        let intent = IngressEffectIntent::Carbons {
            excluded_source: owner.with_resource_str("phone").expect("source"),
            carbon_recipients: vec![target.clone()],
            kind,
        };
        let original = &mut submission.plan.sanitized_message;
        original.bodies.clear();
        original.payloads.push(build_propose(
            xmpp_parsers::jingle::SessionId("signal".into()),
            CallOffer::audio_video(),
        ));
        add_stanza_id(
            original,
            &StanzaId::new("sender-stamp", sender.clone().into()),
        );
        submission.plan.intents.push(intent.clone());
        let decision = commit_submission(&fixture.uow, &submission, 1)
            .await
            .expect("carbon authority");
        let context = SmIngressAppendContext {
            message_key: decision.message_key.expect("key"),
            receipt: crate::ingress::receipt_key(&intent).expect("receipt"),
            received_at: None,
            archive_positions: vec![],
            dispatch_stream: None,
        };
        let original = &submission.plan.sanitized_message;
        let mut inner = original.clone();
        if kind == CarbonKind::Received {
            add_stanza_id(
                &mut inner,
                &StanzaId::new("recipient-stamp", owner.clone().into()),
            );
        }
        assert!(inner.thread.is_none(), "canonical message has no thread");
        for id in ["", " ", "\t\n"] {
            let mut forged_thread = inner.clone();
            forged_thread.payloads.push(
                minidom::Element::builder("thread", waddle_xmpp_core::xep0201::CLIENT_STANZA_NS)
                    .attr(minidom::rxml::xml_ncname!("parent").to_owned(), "forged")
                    .append(id)
                    .build(),
            );
            assert_eq!(
                authority
                    .accept_live_delivery(
                        &context,
                        &target,
                        &wrapper(&forged_thread, &owner, &target, kind),
                        || panic!("invalid thread must not disappear during carbon authorization"),
                    )
                    .await,
                FullJidDeliveryOutcome::MaybeCommitted,
                "{kind:?} carbon with invalid thread {id:?} must be rejected",
            );
        }
        let mut changed_payload = inner.clone();
        changed_payload.payloads[0] = build_propose(
            xmpp_parsers::jingle::SessionId("substituted".into()),
            CallOffer::audio_only(),
        );
        let mut wrong_owner = original.clone();
        add_stanza_id(
            &mut wrong_owner,
            &StanzaId::new(
                "foreign-stamp",
                "mallory@example.com".parse().expect("foreign owner"),
            ),
        );
        let mut changed_sender = inner.clone();
        add_stanza_id(
            &mut changed_sender,
            &StanzaId::new("forged-sender", sender.into()),
        );
        let mut duplicate_owner = inner.clone();
        duplicate_owner.payloads.push(build_stanza_id_element(
            "extra-owner-stamp",
            &owner.clone().into(),
        ));
        for invalid in [
            changed_payload,
            wrong_owner,
            changed_sender,
            duplicate_owner,
        ] {
            assert_eq!(
                authority
                    .accept_live_delivery(
                        &context,
                        &target,
                        &wrapper(&invalid, &owner, &target, kind),
                        || panic!("forged carbon must not enqueue")
                    )
                    .await,
                FullJidDeliveryOutcome::MaybeCommitted
            );
        }
        assert_eq!(
            authority
                .accept_live_delivery(
                    &context,
                    &target,
                    &wrapper(&inner, &owner, &target, kind),
                    || BroadcastOutcome::Delivered
                )
                .await,
            FullJidDeliveryOutcome::Delivered
        );
    }
    assert_eq!(fixture.count("ingress_send_attempts").await, 2);
    authority.drain_and_join(Duration::from_secs(1)).await;
    drop(authority);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_archive_free_carbon_stamp_is_limited_to_received_owner() {
    archive_free_carbon_stamps(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_archive_free_carbon_stamp_is_limited_to_received_owner() {
    if let Some(fixture) = IngressFixture::postgres("archive_free_carbon").await {
        archive_free_carbon_stamps(fixture).await;
    }
}

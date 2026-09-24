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
        for invalid in [
            wrapper(original, &owner, &target, wrong_direction),
            wrapper(original, &owner, &excluded, kind),
            wrapper(&edited, &owner, &target, kind),
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

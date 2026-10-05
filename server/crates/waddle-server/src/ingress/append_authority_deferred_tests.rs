//! A relayed claim's canonical read is deferred to its first trusting consumer
//! and runs once per context clone-tree, caching a rejection (#1790).
use super::*;
use crate::ingress::{
    commit::commit_submission, identity::IngressAppendObligationRef, test_support::IngressFixture,
};
use waddle_xmpp::ingress::{EffectMessageIdentity, IngressEffectIntent};

async fn deferred_authority_reads_once(fixture: IngressFixture) {
    let mut submission = fixture.submission(None, "deferred authority");
    let recipient: jid::FullJid = "juliet@example.com/phone".parse().expect("recipient");
    let intent = IngressEffectIntent::RouteDirect {
        recipient: recipient.to_bare(),
        fanout: vec![recipient.clone()],
        route_identity: EffectMessageIdentity::capture_ordinal(0),
    };
    let receipt = crate::ingress::receipt_key(&intent).expect("receipt");
    submission.plan.intents = vec![intent];
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("canonical row");
    let authorized = IngressAppendObligationRef {
        archive_positions: Vec::new(),
        dispatch_stream: None,
        message_key: decision.message_key.expect("canonical key"),
        sender_bare: submission.sender.to_bare(),
        receipt,
        received_at: None,
    };
    let mut message = submission.plan.sanitized_message.clone();
    message.to = Some(recipient.into());
    let stanza = Stanza::Message(message);

    let context = authorized.clone().into_deferred_context(fixture.db.clone());
    assert_eq!(canonical_reads::count(), 0, "construction never reads");
    let forwarded = context.clone();
    context
        .ensure_verified(&stanza)
        .await
        .expect("canonical row authorizes the claim");
    forwarded
        .ensure_verified(&stanza)
        .await
        .expect("a clone shares the resolved authority");
    assert_eq!(canonical_reads::count(), 1, "one read per clone-tree");

    let mut absent = authorized;
    absent.message_key = waddle_xmpp::ingress::MessageKey::new();
    let context = absent.into_deferred_context(fixture.db.clone());
    let fallback = context.clone();
    assert!(context.ensure_verified(&stanza).await.is_err());
    assert!(
        fallback.ensure_verified(&stanza).await.is_err(),
        "the rejection is cached, never re-read into success"
    );
    assert_eq!(canonical_reads::count(), 2, "the rejection is read once");

    assert!(AppendAuthority::Verified
        .ensure_verified(&stanza)
        .await
        .is_ok());
    assert_eq!(canonical_reads::count(), 2, "verified contexts never read");
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_deferred_authority_reads_once_and_caches_rejection() {
    deferred_authority_reads_once(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_deferred_authority_reads_once_and_caches_rejection() {
    if let Some(fixture) = IngressFixture::postgres("deferred_append_authority").await {
        deferred_authority_reads_once(fixture).await;
    }
}

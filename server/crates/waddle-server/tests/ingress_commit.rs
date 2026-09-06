mod ingress_support;

use ingress_support::IngressFixture;
use waddle_server::{
    ingress::{
        commit::commit_submission,
        effects::{direct::DurableDirectEffect, Effect},
        DurableEffect, IngressDecisionClass, IngressSubmission, PlannedEffect,
    },
    ingress_substrate::MessageEnvelope,
    ingress_uow::CanonicalMessageRepository,
};
use waddle_xmpp::{
    ingress::IngressEffectIntent,
    mam::{ArchiveExpectation, ArchivedMessage},
};
use waddle_xmpp_core::xep0359::StanzaId;

fn archive_plan(
    fixture: &IngressFixture,
    origin: Option<&str>,
    body: &str,
    id: &str,
) -> IngressSubmission {
    let mut submission = fixture.submission(origin, body);
    let archive = fixture.principal.bare_jid().clone();
    let stanza_id = StanzaId::new(id, archive.clone().into());
    let mut message = ArchivedMessage::for_test(
        submission
            .plan
            .sanitized_message
            .from
            .clone()
            .expect("sender"),
        submission
            .plan
            .sanitized_message
            .to
            .clone()
            .expect("recipient"),
    );
    message.id = id.to_owned();
    message.body = Some(body.to_owned());
    message.message_type = xmpp_parsers::message::MessageType::Chat;
    message.stanza_id = Some(stanza_id.clone());
    message.origin_id = submission.digest_input.origin().cloned();
    submission
        .plan
        .intents
        .push(IngressEffectIntent::ArchiveAuthoritative {
            archive: archive.clone(),
            stanza_id,
            by: archive.clone(),
            archived_at: message.timestamp,
        });
    submission
        .plan
        .plan
        .push(PlannedEffect::new(Effect::Durable(DurableEffect::Direct(
            DurableDirectEffect::ArchiveDirect {
                archive,
                message: Box::new(message),
                archive_expectation: ArchiveExpectation::Fresh,
            },
        ))));
    submission
}

#[path = "ingress_cases/canonical.rs"]
mod canonical;
#[path = "ingress_cases/execution.rs"]
mod execution;
#[path = "ingress_cases/fencing.rs"]
mod fencing;
#[path = "ingress_cases/moderation.rs"]
mod moderation;
#[path = "ingress_cases/projections.rs"]
mod projections;
#[path = "ingress_cases/reconciliation.rs"]
mod reconciliation;
#[path = "ingress_cases/rejections.rs"]
mod rejections;
#[path = "ingress_cases/stream.rs"]
mod stream;
#[path = "ingress_cases/tombstone.rs"]
mod tombstone;

//! Shared extension admission setup for the dedicated offline recovery suites.
use crate::ingress_support::IngressFixture;
use waddle_extensions::PluginId;
use waddle_server::{
    ingress::{ExtensionPrincipal, IngressPrincipal, IngressStreamIdentity, IngressSubmission},
    ingress_uow::{ConfiguredPluginGrants, ExtensionGrantRepository},
};
use waddle_xmpp::ingress::{DigestContext, DigestInput, TransportGeneration};

use waddle_server::{
    ingress::{
        effects::{
            delivery::{ExternalDeliveryEffect, PreparedOfflineNotification},
            Effect, ExternalEffect,
        },
        PlannedEffect,
    },
    notification_outbox::NotificationCandidate,
};
use waddle_xmpp::{
    ingress::{
        IngressEffectIntent, NotificationActivityMutation, NotificationCandidateOutcome,
        PendingDeliveryMutation,
    },
    pending_delivery::{PendingPayload, PendingRow, PendingRowId},
};

pub async fn extension_submission(
    fixture: &IngressFixture,
    origin: &str,
    body: &str,
) -> IngressSubmission {
    let plugin = PluginId::new("offline-recovery-plugin").expect("plugin");
    let mut tx = fixture.uow.begin().await.expect("grant transaction");
    ExtensionGrantRepository::sync_configured(
        &mut tx,
        &[ConfiguredPluginGrants {
            plugin: plugin.clone(),
            can_send: true,
            provider_rooms: vec![],
        }],
    )
    .await
    .expect("configure send grant");
    let grant = ExtensionGrantRepository::active_send_grant(&mut tx, &plugin)
        .await
        .expect("resolve send grant")
        .expect("active grant");
    tx.commit().await.expect("persist grant");
    let mut submission = fixture.submission(Some(origin), body);
    let requester = fixture.principal.bare_jid().clone();
    submission.identity = IngressStreamIdentity::Extension {
        plugin,
        requester: Some(requester.clone()),
    };
    submission.principal = IngressPrincipal::Extension(ExtensionPrincipal {
        grant,
        requester: Some(requester.clone()),
        sender: requester.clone(),
    });
    submission.sender = requester
        .with_resource_str("extension-host")
        .expect("host sender");
    submission.plan.sanitized_message.from = Some(submission.sender.clone().into());
    submission.connection_generation = TransportGeneration::Host;
    submission.digest_input = DigestInput::from_parsed(
        &submission.plan.sanitized_message,
        &DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![requester],
            stanza_lang: None,
        },
    )
    .expect("extension digest");
    submission
}

pub async fn revoke_after_commit(fixture: &IngressFixture, submission: &IngressSubmission) {
    let IngressPrincipal::Extension(_) = &submission.principal else {
        panic!("extension submission");
    };
    let mut tx = fixture.uow.begin().await.expect("revoke transaction");
    assert_eq!(
        ExtensionGrantRepository::sync_configured(&mut tx, &[])
            .await
            .expect("revoke grant")
            .revoked,
        1
    );
    tx.commit().await.expect("persist revocation");
}

pub fn offline_submission(mut submission: IngressSubmission, origin: &str) -> IngressSubmission {
    let recipient: jid::BareJid = "juliet@example.com".parse().expect("recipient");
    let stamp = waddle_xmpp_core::xep0359::StanzaId::new(origin, recipient.clone().into());
    let row = PendingRow {
        id: PendingRowId::fresh(),
        recipient: recipient.clone(),
        original_receipt_at: chrono::Utc::now(),
        payload: PendingPayload::Archived(stamp.clone()),
        flushed_in_session: None,
        outbound_sequence: None,
    };
    let candidate = NotificationCandidate::direct_message(
        recipient.clone(),
        submission.sender.clone().into(),
        stamp.clone(),
        false,
    )
    .expect("direct candidate");
    submission.plan.intents.extend([
        IngressEffectIntent::PendingDelivery {
            mutation: PendingDeliveryMutation::Archived {
                recipient: recipient.clone(),
                row_id: row.id.clone(),
                archive_stanza_id: stamp.clone(),
            },
        },
        IngressEffectIntent::NotificationActivityPreview {
            owner: recipient.clone(),
            mutation: NotificationActivityMutation::NotificationCandidate {
                conversation: recipient.clone(),
                archive_stanza_id: stamp.clone(),
                outcome: NotificationCandidateOutcome::Inserted,
            },
        },
        IngressEffectIntent::NotificationActivityPreview {
            owner: recipient.clone(),
            mutation: NotificationActivityMutation::OfflineDelivery {
                conversation: recipient,
                archive_stanza_id: stamp,
            },
        },
    ]);
    submission
        .plan
        .plan
        .push(PlannedEffect::new(Effect::External(
            ExternalEffect::Delivery(ExternalDeliveryEffect::QueueOfflineDelivery {
                row,
                prepared_notification: PreparedOfflineNotification::Prepared(Box::new(candidate)),
                original_message: Box::new(submission.plan.sanitized_message.clone()),
            }),
        )));
    submission
}

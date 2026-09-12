//! Restore and finalize the derived archive position under canonical authority.
use crate::ingress_uow::{EffectIntentRepository, IngressUowError, IngressUowTransaction};
use jid::BareJid;
use waddle_xmpp::{
    ingress::{IngressEffectIntent, MessageKey},
    mam::{ArchiveExpectation, ArchivedMessage, MamTxStoreOutcome},
};

pub(super) fn restore(
    planned: &mut [IngressEffectIntent],
    recorded: &[IngressEffectIntent],
) -> Result<(), IngressUowError> {
    for (index, intent) in recorded.iter().enumerate() {
        for other in &recorded[index + 1..] {
            if intent.semantic_key() == other.semantic_key() {
                check_ordinals(intent, other)?;
            }
        }
    }
    for intent in planned {
        let authority = intent.authority_key();
        let Some(saved) = recorded.iter().find(|row| row.authority_key() == authority) else {
            continue;
        };
        check_ordinals(intent, saved)?;
        if let (
            IngressEffectIntent::ArchiveAuthoritative {
                archived_at,
                ordinal,
                ..
            }
            | IngressEffectIntent::SystemMessageArchive {
                archived_at,
                ordinal,
                ..
            },
            IngressEffectIntent::ArchiveAuthoritative {
                archived_at: saved_at,
                ordinal: saved_ordinal,
                ..
            }
            | IngressEffectIntent::SystemMessageArchive {
                archived_at: saved_at,
                ordinal: saved_ordinal,
                ..
            },
        ) = (intent, saved)
        {
            *archived_at = *saved_at;
            *ordinal = *saved_ordinal;
        }
    }
    Ok(())
}

fn check_ordinals(
    intent: &IngressEffectIntent,
    saved: &IngressEffectIntent,
) -> Result<(), IngressUowError> {
    match (intent, saved) {
        (
            IngressEffectIntent::ArchiveAuthoritative {
                ordinal: Some(stored),
                ..
            }
            | IngressEffectIntent::SystemMessageArchive {
                ordinal: Some(stored),
                ..
            },
            IngressEffectIntent::ArchiveAuthoritative {
                ordinal: Some(recorded),
                ..
            }
            | IngressEffectIntent::SystemMessageArchive {
                ordinal: Some(recorded),
                ..
            },
        ) if stored != recorded => Err(IngressUowError::ArchiveOrdinalConflict {
            recorded: *recorded,
            stored: *stored,
        }),
        _ => Ok(()),
    }
}

pub(super) fn expectation(
    key: MessageKey,
    recorded: &[IngressEffectIntent],
    archive: &BareJid,
    message: &ArchivedMessage,
) -> ArchiveExpectation {
    recorded.iter().find_map(|intent| match intent {
        IngressEffectIntent::ArchiveAuthoritative { archive: saved, stanza_id, archived_at, ordinal, .. }
        | IngressEffectIntent::SystemMessageArchive { archive: saved, stanza_id, archived_at, ordinal, .. }
            if saved == archive && stanza_id.id == message.id => {
                if ordinal.is_none() {
                    tracing::warn!(message_key = ?key, %archive, "recorded archive authority has no ordinal; missing-row repair allocates a position");
                }
                Some(ArchiveExpectation::Existing {
                    stanza_id: stanza_id.clone(),
                    archived_at: *archived_at,
                    ordinal: *ordinal,
                })
            }
        _ => None,
    }).unwrap_or(ArchiveExpectation::Fresh)
}

pub(super) async fn finalize(
    tx: &mut IngressUowTransaction<'_>,
    key: MessageKey,
    intents: &[IngressEffectIntent],
    archive: &BareJid,
    message: &ArchivedMessage,
    outcome: &MamTxStoreOutcome,
) -> Result<(), IngressUowError> {
    let ordinal = match outcome {
        MamTxStoreOutcome::Inserted { ordinal, .. }
        | MamTxStoreOutcome::Existing { ordinal, .. }
        | MamTxStoreOutcome::Repaired { ordinal, .. } => *ordinal,
        MamTxStoreOutcome::TombstoneHit(_) | MamTxStoreOutcome::Expired(_) => return Ok(()),
    };
    for intent in intents {
        if matches!(intent,
            IngressEffectIntent::ArchiveAuthoritative { archive: saved, stanza_id, .. }
            | IngressEffectIntent::SystemMessageArchive { archive: saved, stanza_id, .. }
            if saved == archive && stanza_id.id == message.id
        ) {
            EffectIntentRepository::record_archive_ordinal(
                tx,
                key,
                &intent.semantic_key(),
                ordinal,
            )
            .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;

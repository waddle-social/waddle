//! Transactional writes and their exact durable completion receipts.
use super::decision::EffectReceiptKey;
use crate::{
    ingress_substrate::EffectReceiptKind,
    ingress_uow::{
        EffectReceiptRepository, InboxRepository, IngressUowError, IngressUowTransaction,
        MamArchiveRepository,
    },
    server::routes::interpret::effects::{
        direct::DurableDirectEffect, room::DurableRoomEffect, AppliedDurableEffects, DurableEffect,
        DurableOutcome, Effect, ExternalEffect, IngressPlan, ProjectionRef,
    },
};
use jid::BareJid;
use sha2::{Digest, Sha256};
use waddle_xmpp::{
    ingress::{IngressEffectIntent, MessageKey},
    mam::{ArchiveExpectation, MamTxStoreOutcome},
};
pub(super) struct AppliedDurable {
    pub archives: Vec<(BareJid, MamTxStoreOutcome)>,
    pub outcomes: AppliedDurableEffects,
    pub receipts: Vec<EffectReceiptKey>,
}

pub(crate) fn receipt_key(
    intent: &IngressEffectIntent,
) -> Result<EffectReceiptKey, IngressUowError> {
    let kind = intent.with_encoded_v1(|kind, _| kind)?;
    Ok(EffectReceiptKey {
        kind: EffectReceiptKind::from_storage(kind),
        semantic_identity_hash: Sha256::digest(intent.semantic_key().storage_identity().as_bytes())
            .into(),
    })
}
pub(super) fn external_receipts(
    external: &[ExternalEffect],
    intents: &[IngressEffectIntent],
) -> Result<Vec<Vec<EffectReceiptKey>>, IngressUowError> {
    super::receipts::external_receipts(external, intents)
}
pub(super) async fn apply_durable(
    tx: &mut IngressUowTransaction<'_>,
    key: MessageKey,
    plan: &IngressPlan,
    recorded: &[IngressEffectIntent],
    room_proof: &super::commit_room::RoomProof<'_>,
) -> Result<AppliedDurable, IngressUowError> {
    let mut applied = AppliedDurable {
        archives: Vec::new(),
        outcomes: AppliedDurableEffects::default(),
        receipts: Vec::new(),
    };
    let mut completed = Vec::new();
    for (index, planned) in plan.plan.iter().enumerate() {
        let Effect::Durable(effect) = &planned.effect else {
            continue;
        };
        if matches!(
            effect,
            DurableEffect::Room(DurableRoomEffect::ProjectGroupchatInbox { .. })
                | DurableEffect::Direct(
                    DurableDirectEffect::ProjectInbox { .. }
                        | DurableDirectEffect::DmCallThreadProjection { .. }
                        | DurableDirectEffect::MarkInboxRead { .. }
                )
        ) && !plan
            .intents
            .iter()
            .any(|intent| corresponds(effect, intent))
        {
            continue;
        }
        let archive_effect = matches!(
            effect,
            DurableEffect::Direct(DurableDirectEffect::ArchiveDirect { .. })
                | DurableEffect::Room(DurableRoomEffect::ArchiveGroupchat { .. })
        );
        if !archive_effect && super::suppression::tombstone_swallowed(planned, &applied.archives) {
            // Retraction deliberately discharges this obligation without applying
            // it. Receipt it too, including intents introduced by a duplicate replan.
            completed.push(effect);
            continue;
        }
        match effect {
            DurableEffect::Direct(DurableDirectEffect::ArchiveDirect {
                archive, message, ..
            })
            | DurableEffect::Room(DurableRoomEffect::ArchiveGroupchat {
                room: archive,
                message,
                ..
            }) => {
                let expectation = recorded
                    .iter()
                    .find_map(|intent| match intent {
                        IngressEffectIntent::ArchiveAuthoritative {
                            archive: stored,
                            stanza_id,
                            archived_at,
                            ..
                        }
                        | IngressEffectIntent::SystemMessageArchive {
                            archive: stored,
                            stanza_id,
                            archived_at,
                            ..
                        } if stored == archive && stanza_id.id == message.id => {
                            Some(ArchiveExpectation::Existing {
                                stanza_id: stanza_id.clone(),
                                archived_at: *archived_at,
                            })
                        }
                        _ => None,
                    })
                    .unwrap_or(ArchiveExpectation::Fresh);
                #[cfg(feature = "clustering")]
                let outcome = if matches!(effect, DurableEffect::Room(_))
                    && matches!(
                        tx.fencing(),
                        crate::ingress_uow::IngressFencing::Clustered(_)
                    ) {
                    MamArchiveRepository::store_fenced(
                        tx,
                        room_proof
                            .as_ref()
                            .ok_or(IngressUowError::ClaimFenceMissing)?,
                        archive,
                        message,
                        expectation,
                    )
                    .await?
                } else {
                    MamArchiveRepository::store(tx, archive, message, expectation).await?
                };
                #[cfg(not(feature = "clustering"))]
                let outcome = {
                    let _ = room_proof;
                    MamArchiveRepository::store(tx, archive, message, expectation).await?
                };
                applied.archives.push((archive.clone(), outcome));
            }
            DurableEffect::Direct(DurableDirectEffect::ProjectInbox {
                owner,
                entry,
                increment_unread,
            }) => {
                let entry =
                    InboxRepository::apply_once(tx, key, owner, *entry.clone(), *increment_unread)
                        .await?;
                applied
                    .outcomes
                    .insert(ProjectionRef(index), DurableOutcome::Inbox(entry));
            }
            DurableEffect::Room(DurableRoomEffect::ProjectGroupchatInbox {
                owner,
                entry,
                is_recipient,
                recovery,
                ..
            }) => {
                let entry = if let Some(recovery) = recovery.as_ref() {
                    if recovery_completed(tx, key, recovery, recorded).await? {
                        InboxRepository::apply_once(tx, key, owner, *entry.clone(), *is_recipient)
                            .await?
                    } else {
                        InboxRepository::upsert_with_groupchat_notification_recovery(
                            tx,
                            key,
                            owner,
                            *entry.clone(),
                            *is_recipient,
                            recovery.clone(),
                        )
                        .await?
                    }
                } else {
                    InboxRepository::apply_once(tx, key, owner, *entry.clone(), *is_recipient)
                        .await?
                };
                applied
                    .outcomes
                    .insert(ProjectionRef(index), DurableOutcome::Inbox(entry));
            }

            DurableEffect::Direct(DurableDirectEffect::MarkInboxRead {
                owner,
                channel,
                thread,
            }) => {
                if let Some(entry) =
                    InboxRepository::mark_read(tx, key, owner, channel, thread.as_ref()).await?
                {
                    applied
                        .outcomes
                        .insert(ProjectionRef(index), DurableOutcome::Inbox(entry));
                }
            }
            DurableEffect::Direct(DurableDirectEffect::RetractionTombstone {
                archive,
                target,
                tombstone,
            }) => {
                MamArchiveRepository::replace_with_tombstone(tx, archive, target, tombstone).await?
            }
            DurableEffect::Direct(DurableDirectEffect::DmCallThreadProjection {
                owner,
                mutation,
            }) => InboxRepository::apply_call_thread(tx, key, owner, mutation).await?,
        }
        completed.push(effect);
    }
    for intent in &plan.intents {
        if durable_intent_complete(&completed, intent) {
            let receipt = receipt_key(intent)?;
            EffectReceiptRepository::record_receipt(
                tx,
                key,
                receipt.kind,
                &receipt.semantic_identity_hash,
            )
            .await?;
            if !applied.receipts.contains(&receipt) {
                applied.receipts.push(receipt);
            }
        }
    }
    Ok(applied)
}
fn corresponds(effect: &DurableEffect, intent: &IngressEffectIntent) -> bool {
    match (effect, intent) {
        (
            DurableEffect::Direct(DurableDirectEffect::ArchiveDirect {
                archive, message, ..
            }),
            IngressEffectIntent::ArchiveAuthoritative {
                archive: stored,
                stanza_id,
                ..
            }
            | IngressEffectIntent::SystemMessageArchive {
                archive: stored,
                stanza_id,
                ..
            },
        )
        | (
            DurableEffect::Room(DurableRoomEffect::ArchiveGroupchat {
                room: archive,
                message,
                ..
            }),
            IngressEffectIntent::ArchiveAuthoritative {
                archive: stored,
                stanza_id,
                ..
            }
            | IngressEffectIntent::SystemMessageArchive {
                archive: stored,
                stanza_id,
                ..
            },
        ) => archive == stored && message.id == stanza_id.id,
        (
            DurableEffect::Direct(DurableDirectEffect::ProjectInbox {
                owner,
                entry,
                increment_unread,
            }),
            IngressEffectIntent::InboxProject {
                owner: stored,
                mutation:
                    waddle_xmpp::ingress::InboxProjectionMutation::Direct {
                        entry: saved,
                        increment_unread: saved_unread,
                    },
            },
        ) => owner == stored && entry.as_ref() == saved && increment_unread == saved_unread,
        (
            DurableEffect::Direct(DurableDirectEffect::DmCallThreadProjection { owner, mutation }),
            IngressEffectIntent::InboxProject {
                owner: stored,
                mutation: saved,
            },
        ) => owner == stored && mutation.as_ref() == saved,
        (
            DurableEffect::Direct(DurableDirectEffect::MarkInboxRead {
                owner,
                channel,
                thread,
            }),
            IngressEffectIntent::InboxProject {
                owner: stored,
                mutation,
            },
        ) => {
            owner == stored
                && match mutation {
                    waddle_xmpp::ingress::InboxProjectionMutation::GroupchatChannelRead {
                        room,
                    } => room == channel && thread.is_none(),
                    waddle_xmpp::ingress::InboxProjectionMutation::GroupchatThreadRead {
                        room,
                        thread_id,
                    } => room == channel && thread.as_ref() == Some(thread_id),
                    _ => false,
                }
        }
        (
            DurableEffect::Room(DurableRoomEffect::ProjectGroupchatInbox {
                owner,
                entry,
                is_recipient,
                ..
            }),
            IngressEffectIntent::InboxProject {
                owner: stored,
                mutation,
            },
        ) => {
            owner == stored
                && match mutation {
                    waddle_xmpp::ingress::InboxProjectionMutation::GroupchatChannel {
                        room,
                        increment_unread,
                    } => {
                        room == &entry.partner
                            && entry.thread_id.is_none()
                            && increment_unread == is_recipient
                    }
                    waddle_xmpp::ingress::InboxProjectionMutation::GroupchatThread {
                        room,
                        thread_id,
                    } => {
                        room == &entry.partner
                            && entry.thread_id.as_deref() == Some(thread_id.as_str())
                    }
                    waddle_xmpp::ingress::InboxProjectionMutation::GroupchatChannelAndThread {
                        room,
                        thread_id,
                        increment_unread,
                    } => {
                        room == &entry.partner
                            && increment_unread == is_recipient
                            && (entry.thread_id.is_none()
                                || entry.thread_id.as_deref() == Some(thread_id.as_str()))
                    }
                    _ => false,
                }
        }
        (
            DurableEffect::Direct(DurableDirectEffect::RetractionTombstone {
                archive, target, ..
            }),
            IngressEffectIntent::RetractionTombstone { mutation },
        ) => archive == &mutation.archive && target == &mutation.target_stanza_id,
        (
            DurableEffect::Room(DurableRoomEffect::ProjectGroupchatInbox {
                recovery: Some(recovery),
                ..
            }),
            IngressEffectIntent::GroupchatNotificationRecovery { mutation },
        ) => {
            mutation.action == waddle_xmpp::ingress::GroupchatNotificationRecoveryAction::Recorded
                && mutation.recipient == recovery.key.recipient
                && mutation.room == recovery.key.room
                && mutation.thread_id.as_ref().map(|thread| thread.as_str())
                    == recovery.key.thread_id.as_deref()
                && mutation.archive_stanza_id == recovery.key.archive_stanza_id
                && mutation.sender == recovery.sender_jid
                && mutation.is_live_occupant == recovery.is_live_occupant
                && mutation.room_members_only == recovery.room_members_only
                && mutation.sender_can_broadcast_channel_mention
                    == recovery.sender_can_broadcast_channel_mention
                && mutation.created_at_ms == recovery.created_at_ms
        }
        _ => false,
    }
}

async fn recovery_completed(
    tx: &mut IngressUowTransaction<'_>,
    key: MessageKey,
    recovery: &waddle_xmpp::inbox::storage::GroupchatNotificationRecovery,
    recorded: &[IngressEffectIntent],
) -> Result<bool, IngressUowError> {
    for intent in recorded {
        if let IngressEffectIntent::GroupchatNotificationRecovery { mutation } = intent {
            if mutation.action
                == waddle_xmpp::ingress::GroupchatNotificationRecoveryAction::Completed
                && mutation.recipient == recovery.key.recipient
                && mutation.room == recovery.key.room
                && mutation.archive_stanza_id == recovery.key.archive_stanza_id
                && mutation.thread_id.as_ref().map(|id| id.as_str())
                    == recovery.key.thread_id.as_deref()
            {
                let receipt = receipt_key(intent)?;
                if EffectReceiptRepository::contains(
                    tx,
                    key,
                    receipt.kind,
                    &receipt.semantic_identity_hash,
                )
                .await?
                {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

fn durable_intent_complete(completed: &[&DurableEffect], intent: &IngressEffectIntent) -> bool {
    let matching = completed
        .iter()
        .copied()
        .filter(|effect| corresponds(effect, intent))
        .collect::<Vec<_>>();
    if matches!(
        intent,
        IngressEffectIntent::InboxProject {
            mutation: waddle_xmpp::ingress::InboxProjectionMutation::GroupchatChannelAndThread { .. },
            ..
        }
    ) {
        let channel = matching.iter().any(|effect| matches!(effect, DurableEffect::Room(DurableRoomEffect::ProjectGroupchatInbox { entry, .. }) if entry.thread_id.is_none()));
        let thread = matching.iter().any(|effect| matches!(effect, DurableEffect::Room(DurableRoomEffect::ProjectGroupchatInbox { entry, .. }) if entry.thread_id.is_some()));
        channel && thread
    } else {
        !matching.is_empty()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inbox_receipt_requires_the_applied_projection_payload() {
        let owner: BareJid = "owner@example.test".parse().expect("owner");
        let peer: BareJid = "peer@example.test".parse().expect("peer");
        let entry = waddle_xmpp::inbox::InboxEntry::new(
            peer,
            waddle_xmpp::inbox::ConversationKind::Direct,
            "id",
            1,
        );
        let effect = DurableEffect::Direct(DurableDirectEffect::ProjectInbox {
            owner: owner.clone(),
            entry: Box::new(entry.clone()),
            increment_unread: true,
        });
        let mut intent = IngressEffectIntent::InboxProject {
            owner,
            mutation: waddle_xmpp::ingress::InboxProjectionMutation::Direct {
                entry,
                increment_unread: true,
            },
        };
        assert!(corresponds(&effect, &intent));
        if let IngressEffectIntent::InboxProject {
            mutation:
                waddle_xmpp::ingress::InboxProjectionMutation::Direct {
                    increment_unread, ..
                },
            ..
        } = &mut intent
        {
            *increment_unread = false;
        }
        assert!(!corresponds(&effect, &intent));
    }
}

use std::collections::{BTreeSet, HashMap};

use jid::BareJid;
use thiserror::Error;
use waddle_client_session::judgments::SafetyCategory;
use waddle_client_session::{
    AuthorIdentityPolicy, Delivery, HistoryPage, LocalEcho, ReplyTo, Timeline, TimelineItem,
};

use crate::adapter::{adapt, display_name, Metadata};
use crate::dto::{
    Attachment, Category, DeliveryUpdate, Encryption, Item, Reaction, ReplyPreview, Score, Scores,
    TimelineCommand, TimelineSnapshot,
};

/// A rejected application-store operation, distinct from an XMPP stanza error.
#[derive(Debug, Error)]
pub enum TimelineError {
    #[error("Timeline belongs to another account; reset it before changing accounts")]
    AccountMismatch,
    #[error("Conversation kind changed without resetting the timeline")]
    ConversationKindMismatch,
    #[error("Conversation has no timeline")]
    MissingConversation,
    #[error("An origin identifier must not be empty")]
    EmptyOriginId,
}

#[derive(Debug)]
struct Conversation {
    timeline: Timeline,
    metadata: Vec<Metadata>,
    group: bool,
    own_nick: Option<String>,
}

/// One account's timelines, owned by a native transport or browser adapter.
#[derive(Debug, Default)]
pub struct TimelineStore {
    account: Option<BareJid>,
    conversations: HashMap<BareJid, Conversation>,
}

impl TimelineStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply an operation without touching a network, runtime, clock, or device.
    ///
    /// # Errors
    /// Rejects cross-account use, a changed conversation kind, missing timelines,
    /// and local sends without an origin identifier.
    pub fn execute(&mut self, command: TimelineCommand) -> Result<TimelineSnapshot, TimelineError> {
        match command {
            TimelineCommand::TimelineReset {} => {
                *self = Self::default();
                Ok(TimelineSnapshot::default())
            }
            TimelineCommand::TimelineSnapshot { conversation_jid } => Ok(self
                .conversations
                .get(&conversation_jid)
                .map_or_else(TimelineSnapshot::default, snapshot)),
            TimelineCommand::TimelineIngest(input) => {
                self.bind_account(&input.account_jid)?;
                let conversation = self.conversation(
                    &input.conversation_jid,
                    input.group,
                    input.own_nick.clone(),
                )?;
                let mut messages = Vec::new();
                for message in &input.messages {
                    let Some(adapted) = adapt(message, &input) else {
                        continue;
                    };
                    conversation.metadata.push(adapted.metadata);
                    messages.push(adapted.incoming);
                }
                if input.archived {
                    messages.sort_by_key(|message| message.timestamp_ms);
                    let _ = conversation.timeline.apply_history_page(
                        messages,
                        input.cursor,
                        if input.complete {
                            HistoryPage::Complete
                        } else {
                            HistoryPage::Partial
                        },
                    );
                } else {
                    for message in messages {
                        let _ = conversation.timeline.apply_live(message);
                    }
                }
                Ok(snapshot(conversation))
            }
            TimelineCommand::TimelineLocalEcho(input) => {
                if input.origin_id.is_empty() {
                    return Err(TimelineError::EmptyOriginId);
                }
                self.bind_account(&input.account_jid)?;
                let sender = input
                    .own_nick
                    .clone()
                    .unwrap_or_else(|| display_name(&input.account_jid));
                let conversation =
                    self.conversation(&input.conversation_jid, input.group, input.own_nick)?;
                conversation.metadata.push(Metadata {
                    ids: vec![input.origin_id.clone()],
                    sender: sender.clone(),
                    author_id: None,
                    origin_id: Some(input.origin_id.clone()),
                    message_id: Some(input.origin_id.clone()),
                    archive_id: None,
                    archive_by: None,
                    marker_requested: false,
                    thread: input.thread,
                    parent_thread_id: input.parent_thread_id,
                });
                let _ = conversation.timeline.add_local_echo(LocalEcho {
                    reference_id: (!input.group).then(|| input.origin_id.clone()),
                    origin_id: input.origin_id,
                    sender,
                    body: input.body,
                    timestamp_ms: input.timestamp_ms,
                    reply_to: input.reply_to.map(|reply| ReplyTo {
                        id: reply.id,
                        sender: reply.sender,
                    }),
                    attachments: input
                        .attachments
                        .into_iter()
                        .map(|file| waddle_client_session::Attachment {
                            url: file.url,
                            name: file.name,
                            media_type: file.media_type,
                            size: file.size,
                            inline: file.inline,
                            encryption: file.encryption.map(|value| {
                                waddle_client_session::Encryption {
                                    cipher: value.cipher,
                                    key_b64: value.key_b64,
                                    iv_b64: value.iv_b64,
                                    sha256_b64: value.sha256_b64,
                                    sources: value.sources,
                                }
                            }),
                        })
                        .collect(),
                });
                Ok(snapshot(conversation))
            }
            TimelineCommand::TimelineDelivery {
                conversation_jid,
                origin_id,
                delivery,
            } => {
                let conversation = self
                    .conversations
                    .get_mut(&conversation_jid)
                    .ok_or(TimelineError::MissingConversation)?;
                if matches!(delivery, DeliveryUpdate::Rejected) {
                    let _ = conversation.timeline.reject_send(&origin_id);
                } else {
                    let _ = conversation.timeline.set_delivery(
                        &origin_id,
                        match delivery {
                            DeliveryUpdate::Sending => Delivery::Sending,
                            DeliveryUpdate::Sent => Delivery::Sent,
                            DeliveryUpdate::Failed | DeliveryUpdate::Rejected => Delivery::Failed,
                        },
                    );
                }
                Ok(snapshot(conversation))
            }
            TimelineCommand::TimelineOwnReactions {
                conversation_jid,
                target_id,
                emojis,
                own_jid,
                timestamp_ms,
            } => {
                self.bind_account(&own_jid)?;
                let conversation = self
                    .conversations
                    .get_mut(&conversation_jid)
                    .ok_or(TimelineError::MissingConversation)?;
                let sender = conversation
                    .own_nick
                    .clone()
                    .unwrap_or_else(|| display_name(&own_jid));
                let _ = conversation.timeline.apply_own_reaction(
                    &target_id,
                    &sender,
                    &emojis,
                    timestamp_ms,
                );
                Ok(snapshot(conversation))
            }
        }
    }

    fn bind_account(&mut self, account: &BareJid) -> Result<(), TimelineError> {
        match &self.account {
            Some(current) if current != account => Err(TimelineError::AccountMismatch),
            Some(_) => Ok(()),
            None => {
                self.account = Some(account.clone());
                Ok(())
            }
        }
    }

    fn conversation(
        &mut self,
        jid: &BareJid,
        group: bool,
        own_nick: Option<String>,
    ) -> Result<&mut Conversation, TimelineError> {
        let conversation = self
            .conversations
            .entry(jid.clone())
            .or_insert_with(|| Conversation {
                timeline: Timeline::with_author_identity_policy(
                    AuthorIdentityPolicy::KnownTargetRequiresStableIdentity,
                ),
                metadata: Vec::new(),
                group,
                own_nick: own_nick.clone(),
            });
        if conversation.group != group {
            return Err(TimelineError::ConversationKindMismatch);
        }
        if own_nick.is_some() {
            conversation.own_nick = own_nick;
        }
        Ok(conversation)
    }
}

fn snapshot(conversation: &Conversation) -> TimelineSnapshot {
    let mut items: Vec<Item> = conversation
        .timeline
        .items()
        .iter()
        .map(|item| project(item, &conversation.metadata))
        .collect();
    items.sort_by_key(|item| item.timestamp_ms);
    TimelineSnapshot {
        items,
        history_complete: conversation.timeline.history_complete(),
        older_cursor: conversation.timeline.older_cursor().map(str::to_string),
    }
}

fn project(item: &TimelineItem, metadata: &[Metadata]) -> Item {
    let meta = metadata.iter().rev().find(|meta| {
        meta.ids.iter().any(|id| id == item.key.as_str())
            && match (&item.author_id, &meta.author_id) {
                (Some(author), Some(candidate)) => author == candidate,
                _ => item.sender == meta.sender,
            }
    });
    Item {
        id: item.key.as_str().to_string(),
        identity_ids: metadata
            .iter()
            .filter(|meta| {
                meta.ids.iter().any(|id| id == item.key.as_str())
                    && match (&item.author_id, &meta.author_id) {
                        (Some(author), Some(candidate)) => author == candidate,
                        _ => item.sender == meta.sender,
                    }
            })
            .flat_map(|meta| meta.ids.iter().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
        sender: item.sender.clone(),
        author_id: item.author_id.clone(),
        own: item.own,
        body: item.body.clone(),
        timestamp_ms: item.timestamp_ms,
        delivery: match item.delivery {
            Delivery::Sending => "sending",
            Delivery::Sent => "sent",
            Delivery::Failed => "failed",
            _ => "received",
        },
        edited: item.edited,
        retracted: item.retracted,
        reference_id: item.reference_id.clone(),
        correction_id: item.correction_id.clone(),
        reply: item.reply.as_ref().map(|reply| ReplyPreview {
            target_id: reply.target_id.clone(),
            sender: reply.sender.clone(),
            snippet: reply.snippet.clone(),
            target_retracted: reply.target_retracted,
        }),
        reactions: item
            .reactions
            .iter()
            .map(|reaction| Reaction {
                emoji: reaction.emoji.clone(),
                count: reaction.count,
                own: reaction.own,
            })
            .collect(),
        attachments: item
            .attachments
            .iter()
            .map(|file| Attachment {
                url: file.url.clone(),
                name: file.name.clone(),
                media_type: file.media_type.clone(),
                size: file.size,
                inline: file.inline,
                encryption: file.encryption.as_ref().map(|value| Encryption {
                    cipher: value.cipher.clone(),
                    key_b64: value.key_b64.clone(),
                    iv_b64: value.iv_b64.clone(),
                    sha256_b64: value.sha256_b64.clone(),
                    sources: value.sources.clone(),
                }),
            })
            .collect(),
        scores: item.scores.as_ref().map(|scores| Scores {
            model_version: scores.model_version.clone(),
            scores: scores
                .scores
                .iter()
                .filter_map(|score| {
                    let category = match score.category {
                        SafetyCategory::IsQuestion => Category::IsQuestion,
                        SafetyCategory::HateSpeech => Category::HateSpeech,
                        SafetyCategory::Explicit => Category::Explicit,
                        SafetyCategory::Harassment => Category::Harassment,
                        SafetyCategory::Violence => Category::Violence,
                        SafetyCategory::SelfHarm => Category::SelfHarm,
                        SafetyCategory::Spam => Category::Spam,
                        SafetyCategory::Scam => Category::Scam,
                        _ => return None,
                    };
                    Some(Score {
                        category,
                        probability: score.probability.value(),
                        taxonomy_version: score.taxonomy_version.clone(),
                    })
                })
                .collect(),
        }),
        origin_id: meta.and_then(|meta| meta.origin_id.clone()),
        message_id: meta.and_then(|meta| meta.message_id.clone()),
        archive_id: meta.and_then(|meta| meta.archive_id.clone()),
        archive_by: meta.and_then(|meta| meta.archive_by.clone()),
        marker_requested: meta.is_some_and(|meta| meta.marker_requested),
        thread: meta.and_then(|meta| meta.thread.clone()),
        parent_thread_id: meta.and_then(|meta| meta.parent_thread_id.clone()),
    }
}

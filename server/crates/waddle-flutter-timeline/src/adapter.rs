use chrono::DateTime;
use jid::{BareJid, Jid};
use waddle_client_session::judgments::{
    Probability, SafetyCategory, SafetyScore, SafetyScores, SafetyScoresFastening, ScoreTarget,
};
use waddle_client_session::{
    strip_fallback, Attachment, Encryption, IncomingMessage, ReactionUpdate, ReplyTo,
};

use crate::dto::{self, CarbonDirection, Category, Ingest, Message};

#[derive(Debug, Clone)]
pub(crate) struct Metadata {
    pub ids: Vec<String>,
    pub sender: String,
    pub author_id: Option<String>,
    pub origin_id: Option<String>,
    pub message_id: Option<String>,
    pub archive_id: Option<String>,
    pub archive_by: Option<BareJid>,
    pub marker_requested: bool,
    pub thread: Option<String>,
    pub parent_thread_id: Option<String>,
}

pub(crate) struct Adapted {
    pub incoming: IncomingMessage,
    pub metadata: Metadata,
}

pub(crate) fn adapt(message: &Message, input: &Ingest) -> Option<Adapted> {
    let from = message.from.as_ref()?;
    let timestamp_ms = message
        .timestamp
        .as_deref()
        .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
        .map_or(input.received_at_ms, |at| at.timestamp_millis());
    let (sender, own, author_id) = if input.group {
        if from.to_bare() != input.conversation_jid || message.message_type != "groupchat" {
            return None;
        }
        let Some(nick) = from.resource().map(|value| value.as_str().to_string()) else {
            let incoming = if let Some(scores) = message
                .safety_scores
                .as_ref()
                .and_then(|scores| fastening(&input.conversation_jid, scores))
            {
                IncomingMessage {
                    safety_scores: Some(scores),
                    timestamp_ms,
                    ..IncomingMessage::default()
                }
            } else {
                IncomingMessage {
                    moderates_id: Some(message.moderation_target_id.clone()?),
                    timestamp_ms,
                    ..IncomingMessage::default()
                }
            };
            return Some(Adapted {
                incoming,
                metadata: metadata(message, String::new(), None, None, None, false),
            });
        };
        // Archive attribution travels with that archived message. A live
        // attribution is stamped from the presence active at first arrival,
        // never reconstructed from whoever currently holds this nickname.
        let author = if input.archived {
            message.author_real_jid.as_ref()
        } else {
            message.author_stamp.as_ref()
        }
        .map(Jid::to_bare)
        .or_else(|| {
            (!input.archived && input.own_nick.as_deref() == Some(nick.as_str()))
                .then(|| input.account_jid.clone())
        });
        let own = author.as_ref().map_or_else(
            || input.own_nick.as_deref() == Some(nick.as_str()),
            |author| author == &input.account_jid,
        );
        (nick, own, author.map(|jid| jid.to_string()))
    } else {
        if message.message_type != "chat" || message.muc_user || message.muc_pm || message.is_muc {
            return None;
        }
        let own = from.to_bare() == input.account_jid
            || matches!(message.carbon, Some(CarbonDirection::Sent));
        let peer = if own {
            message.to.as_ref()?.to_bare()
        } else {
            from.to_bare()
        };
        if peer != input.conversation_jid || peer == input.account_jid {
            return None;
        }
        let author = if own { &input.account_jid } else { &peer };
        let sender = if own {
            input
                .own_nick
                .clone()
                .unwrap_or_else(|| display_name(author))
        } else {
            display_name(author)
        };
        (sender, own, Some(author.to_string()))
    };
    let archive = archive_identity(message, input);
    let reference_id = if input.group {
        archive.as_ref().map(|(id, _)| id.clone())
    } else {
        message.origin_id.clone().or_else(|| message.id.clone())
    };
    let stanza_id = archive.as_ref().map(|(id, _)| id.clone());
    let score_target = input
        .group
        .then(|| {
            score_target(
                &input.conversation_jid,
                stanza_id.as_deref(),
                message.origin_id.as_deref(),
            )
        })
        .flatten();
    let body = message
        .body
        .as_ref()
        .map(
            |body| match (message.reply_fallback_start, message.reply_fallback_end) {
                (Some(start), Some(end)) => strip_fallback(body, start, end),
                _ => body.clone(),
            },
        )
        .or_else(|| {
            (message.call_thread.is_some()
                || message.call_thread_ended.is_some()
                || message.call_event.is_some())
            .then(String::new)
        });
    let incoming = IncomingMessage {
        stanza_id,
        origin_id: message.origin_id.clone().or_else(|| message.id.clone()),
        message_id: message.id.clone(),
        reference_id,
        sender: sender.clone(),
        author_id: author_id.clone(),
        own,
        body,
        timestamp_ms,
        replaces_id: message.replaces_id.clone(),
        retracts_id: message.retracts_id.clone(),
        // Room moderation is accepted only from the bare room branch above.
        moderates_id: None,
        tombstone: message.is_retracted,
        reply_to: message.reply_to_id.as_ref().map(|id| ReplyTo {
            id: id.clone(),
            sender: if input.group {
                message
                    .reply_to_sender
                    .as_ref()
                    .and_then(|jid| jid.resource().map(|resource| resource.as_str().to_string()))
            } else {
                None
            },
        }),
        reaction: message
            .reaction_target_id
            .as_ref()
            .map(|target_id| ReactionUpdate {
                target_id: target_id.clone(),
                emojis: message.reaction_emojis.clone(),
            }),
        attachments: message.shared_files.iter().map(attachment).collect(),
        score_target,
        safety_scores: None,
    };
    let (archive_id, archive_by) = archive.map_or((None, None), |(id, by)| (Some(id), Some(by)));
    Some(Adapted {
        incoming,
        metadata: metadata(
            message,
            sender,
            author_id,
            archive_id,
            archive_by,
            !input.archived && message.displayed_marker_requested,
        ),
    })
}

fn metadata(
    message: &Message,
    sender: String,
    author_id: Option<String>,
    archive_id: Option<String>,
    archive_by: Option<BareJid>,
    marker_requested: bool,
) -> Metadata {
    Metadata {
        ids: [
            archive_id.clone(),
            message.origin_id.clone(),
            message.id.clone(),
        ]
        .into_iter()
        .flatten()
        .collect(),
        sender,
        author_id,
        origin_id: message.origin_id.clone(),
        message_id: message.id.clone(),
        archive_id,
        archive_by,
        marker_requested,
        thread: message.thread.clone(),
        parent_thread_id: message.parent_thread_id.clone(),
    }
}

fn archive_identity(message: &Message, input: &Ingest) -> Option<(String, BareJid)> {
    if input.archived {
        if let Some(id) = message.mam_id.as_ref().filter(|id| !id.is_empty()) {
            return Some((
                id.clone(),
                if input.group {
                    input.conversation_jid.clone()
                } else {
                    input.account_jid.clone()
                },
            ));
        }
    }
    let accepted = |authority: &Jid| {
        if authority.resource().is_some() {
            return false;
        }
        let authority = authority.to_bare();
        if input.group {
            authority == input.conversation_jid
        } else {
            authority == input.account_jid
                || authority.node().is_none() && authority.domain() == input.account_jid.domain()
        }
    };
    message
        .stanza_ids
        .iter()
        .find(|assigned| !assigned.id.is_empty() && accepted(&assigned.by))
        .map(|assigned| (assigned.id.clone(), assigned.by.to_bare()))
        .or_else(|| {
            let by = message.stanza_id_by.as_ref()?;
            let id = message.stanza_id.as_ref().filter(|id| !id.is_empty())?;
            accepted(by).then(|| (id.clone(), by.to_bare()))
        })
}

pub(crate) fn display_name(jid: &BareJid) -> String {
    jid.node()
        .map_or_else(|| jid.to_string(), |node| node.as_str().to_string())
}

fn attachment(file: &dto::SharedFile) -> Attachment {
    Attachment {
        url: file.url.to_string(),
        name: file.name.clone(),
        media_type: file.media_type.clone(),
        size: file.size,
        inline: file.disposition == "inline",
        encryption: file.encrypted.as_ref().map(|encrypted| Encryption {
            cipher: encrypted.cipher.to_string(),
            key_b64: encrypted.key_b64.clone(),
            iv_b64: encrypted.iv_b64.clone(),
            sha256_b64: encrypted
                .hashes
                .iter()
                .filter(|hash| hash.algo == "sha-256")
                .map(|hash| hash.value_b64.clone())
                .collect(),
            sources: encrypted.sources.iter().map(ToString::to_string).collect(),
        }),
    }
}

fn score_target(room: &BareJid, stanza: Option<&str>, origin: Option<&str>) -> Option<ScoreTarget> {
    Some(ScoreTarget {
        origin_id: origin.filter(|id| !id.is_empty())?.to_string(),
        stanza_id: stanza.filter(|id| !id.is_empty())?.to_string(),
        stanza_by: room.clone(),
    })
}

fn fastening(room: &BareJid, scores: &dto::ScoresFastening) -> Option<SafetyScoresFastening> {
    if &scores.target_stanza_by != room || scores.source_revision_id.is_empty() {
        return None;
    }
    Some(SafetyScoresFastening {
        target: score_target(
            room,
            Some(&scores.target_stanza_id),
            Some(&scores.target_origin_id),
        )?,
        source_revision_id: scores.source_revision_id.clone(),
        scores: SafetyScores {
            model_version: scores.scores.model_version.clone(),
            scores: scores
                .scores
                .scores
                .iter()
                .map(|score| {
                    Some(SafetyScore {
                        category: category(score.category),
                        probability: Probability::try_new(score.probability)?,
                        taxonomy_version: score.taxonomy_version.clone(),
                    })
                })
                .collect::<Option<Vec<_>>>()?,
        },
    })
}

fn category(category: Category) -> SafetyCategory {
    match category {
        Category::IsQuestion => SafetyCategory::IsQuestion,
        Category::HateSpeech => SafetyCategory::HateSpeech,
        Category::Explicit => SafetyCategory::Explicit,
        Category::Harassment => SafetyCategory::Harassment,
        Category::Violence => SafetyCategory::Violence,
        Category::SelfHarm => SafetyCategory::SelfHarm,
        Category::Spam => SafetyCategory::Spam,
        Category::Scam => SafetyCategory::Scam,
    }
}

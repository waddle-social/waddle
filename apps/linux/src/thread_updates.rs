//! Latest fastening state indexed by target, for newly opened threads and
//! root context. Preserve author candidates and exact judgment revisions.

use std::collections::{HashMap, HashSet};
use waddle_client_session::judgments::ScoreTarget;
use waddle_client_session::IncomingMessage;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Author {
    sender: String,
    own: bool,
    identity: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum UpdateKey {
    Correction(Author),
    Retraction(Author),
    Moderation,
    Reaction(Author),
    Judgment(ScoreTarget, String),
}

#[derive(Default)]
pub struct ThreadUpdates {
    by_target: HashMap<String, HashMap<UpdateKey, IncomingMessage>>,
}

impl ThreadUpdates {
    pub fn insert(&mut self, message: IncomingMessage) {
        let author = Author {
            sender: message.sender.clone(),
            own: message.own,
            identity: message.author_id.clone(),
        };
        let (target, key) = if let Some(scores) = &message.safety_scores {
            (
                scores.target.stanza_id.clone(),
                UpdateKey::Judgment(scores.target.clone(), scores.source_revision_id.clone()),
            )
        } else if let Some(reaction) = &message.reaction {
            (reaction.target_id.clone(), UpdateKey::Reaction(author))
        } else if let Some(target) = &message.moderates_id {
            (target.clone(), UpdateKey::Moderation)
        } else if let Some(target) = &message.retracts_id {
            (target.clone(), UpdateKey::Retraction(author))
        } else if let Some(target) = &message.replaces_id {
            if message.body.is_none() {
                return;
            }
            (target.clone(), UpdateKey::Correction(author))
        } else {
            return;
        };
        let updates = self.by_target.entry(target).or_default();
        if updates
            .get(&key)
            .is_none_or(|held| held.timestamp_ms <= message.timestamp_ms)
        {
            updates.insert(key, message);
        }
    }

    pub fn for_message(&self, message: &IncomingMessage) -> Vec<IncomingMessage> {
        let aliases: HashSet<&str> = [
            &message.stanza_id,
            &message.reference_id,
            &message.origin_id,
            &message.message_id,
        ]
        .into_iter()
        .filter_map(|id| id.as_deref())
        .collect();
        let mut updates: Vec<_> = aliases
            .into_iter()
            .filter_map(|id| self.by_target.get(id))
            .flat_map(|updates| updates.values().cloned())
            .collect();
        updates.sort_by_key(|message| message.timestamp_ms);
        updates
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(target: &str, body: &str, at: i64) -> IncomingMessage {
        IncomingMessage {
            replaces_id: Some(target.into()),
            sender: "Alice".into(),
            author_id: Some("alice@example".into()),
            body: Some(body.into()),
            timestamp_ms: at,
            ..IncomingMessage::default()
        }
    }

    #[test]
    fn keeps_latest_per_author_and_target_without_replaying_other_messages() {
        let mut updates = ThreadUpdates::default();
        for at in 0..100 {
            updates.insert(edit("origin", &at.to_string(), at));
        }
        updates.insert(edit("unrelated", "Other message", 101));
        updates.insert(edit("origin", "Stale", 1));
        let root = IncomingMessage {
            stanza_id: Some("stanza".into()),
            origin_id: Some("origin".into()),
            ..IncomingMessage::default()
        };
        let retained = updates.for_message(&root);
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].body.as_deref(), Some("99"));
    }

    #[test]
    fn preserves_real_author_candidates_and_distinct_update_kinds() {
        let mut updates = ThreadUpdates::default();
        updates.insert(edit("origin", "Legitimate", 1));
        let mut spoof = edit("origin", "Forged", 2);
        spoof.author_id = Some("mallory@example".into());
        updates.insert(spoof);
        updates.insert(IncomingMessage {
            moderates_id: Some("stanza".into()),
            timestamp_ms: 3,
            ..IncomingMessage::default()
        });
        updates.insert(IncomingMessage {
            retracts_id: Some("stanza".into()),
            sender: "Mallory".into(),
            timestamp_ms: 4,
            ..IncomingMessage::default()
        });
        let root = IncomingMessage {
            origin_id: Some("origin".into()),
            reference_id: Some("stanza".into()),
            ..IncomingMessage::default()
        };
        assert_eq!(updates.for_message(&root).len(), 4);
    }

    #[test]
    fn judgments_keep_exact_identity_and_each_revision() {
        use waddle_client_session::judgments::{SafetyScores, SafetyScoresFastening};
        let mut updates = ThreadUpdates::default();
        for revision in ["root", "edit"] {
            for at in 0..10 {
                updates.insert(IncomingMessage {
                    timestamp_ms: at,
                    safety_scores: Some(SafetyScoresFastening {
                        target: ScoreTarget {
                            stanza_id: "root".into(),
                            origin_id: "origin".into(),
                            stanza_by: "room@rooms.example".parse().unwrap(),
                        },
                        source_revision_id: revision.into(),
                        scores: SafetyScores {
                            model_version: at.to_string(),
                            scores: vec![],
                        },
                    }),
                    ..IncomingMessage::default()
                });
            }
        }
        let root = IncomingMessage {
            stanza_id: Some("root".into()),
            ..IncomingMessage::default()
        };
        let retained = updates.for_message(&root);
        assert_eq!(retained.len(), 2);
        assert!(retained.iter().all(|update| update.timestamp_ms == 9));
    }
}

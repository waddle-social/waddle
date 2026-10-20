//! Convert already trust-gated XMPP judgment records into timeline input.
//! The expected room must match the claimed stanza authority even though
//! the protocol facade has already verified the fastening's sender.

use jid::BareJid;
use waddle_client_session::judgments::{
    Probability, SafetyCategory, SafetyScore, SafetyScores, SafetyScoresFastening, ScoreTarget,
};
use waddle_xmpp_client_ffi::{WaddleSafetyCategory, WaddleSafetyScoresFastening};

pub fn target(room: &BareJid, stanza: Option<&str>, origin: Option<&str>) -> Option<ScoreTarget> {
    Some(ScoreTarget {
        origin_id: origin.filter(|id| !id.is_empty())?.to_string(),
        stanza_id: stanza.filter(|id| !id.is_empty())?.to_string(),
        stanza_by: room.clone(),
    })
}

pub fn fastening(
    room: &BareJid,
    wire: WaddleSafetyScoresFastening,
) -> Option<SafetyScoresFastening> {
    let authority = wire.target_stanza_by.parse::<BareJid>().ok()?;
    if authority != *room || wire.source_revision_id.is_empty() {
        return None;
    }
    let target = target(
        room,
        Some(&wire.target_stanza_id),
        Some(&wire.target_origin_id),
    )?;
    let scores = wire
        .scores
        .scores
        .into_iter()
        .map(|score| {
            Some(SafetyScore {
                category: category(score.category),
                probability: Probability::new(score.probability)?,
                taxonomy_version: score.taxonomy_version,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(SafetyScoresFastening {
        target,
        source_revision_id: wire.source_revision_id,
        scores: SafetyScores {
            model_version: wire.scores.model_version,
            scores,
        },
    })
}

fn category(category: WaddleSafetyCategory) -> SafetyCategory {
    match category {
        WaddleSafetyCategory::IsQuestion => SafetyCategory::IsQuestion,
        WaddleSafetyCategory::HateSpeech => SafetyCategory::HateSpeech,
        WaddleSafetyCategory::Explicit => SafetyCategory::Explicit,
        WaddleSafetyCategory::Harassment => SafetyCategory::Harassment,
        WaddleSafetyCategory::Violence => SafetyCategory::Violence,
        WaddleSafetyCategory::SelfHarm => SafetyCategory::SelfHarm,
        WaddleSafetyCategory::Spam => SafetyCategory::Spam,
        WaddleSafetyCategory::Scam => SafetyCategory::Scam,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use waddle_xmpp_client_ffi::WaddleSafetyScores;

    #[test]
    fn fastening_must_name_the_expected_room_and_exact_ids() {
        let room: BareJid = "room@rooms.example".parse().unwrap();
        let mut wire = WaddleSafetyScoresFastening {
            target_origin_id: "origin".into(),
            target_stanza_id: "stanza".into(),
            target_stanza_by: room.to_string(),
            source_revision_id: "stanza".into(),
            scores: WaddleSafetyScores {
                model_version: "v1".into(),
                scores: vec![],
            },
        };
        assert!(fastening(&room, wire.clone()).is_some());
        wire.target_stanza_by = "other@rooms.example".into();
        assert!(fastening(&room, wire.clone()).is_none());
        wire.target_stanza_by = room.to_string();
        wire.target_origin_id.clear();
        assert!(fastening(&room, wire).is_none());
    }
}

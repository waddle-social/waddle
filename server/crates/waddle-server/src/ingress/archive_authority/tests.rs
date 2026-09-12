use super::restore;
use crate::ingress::{commit::classify_failure, decision::IngressDecisionClass};
use crate::ingress_uow::IngressUowError;
use waddle_xmpp::{ingress::IngressEffectIntent, mam::ArchiveOrdinal};

fn authority(ordinal: Option<ArchiveOrdinal>) -> IngressEffectIntent {
    let archive: jid::BareJid = "alice@example.org".parse().unwrap();
    IngressEffectIntent::ArchiveAuthoritative {
        stanza_id: waddle_xmpp_core::xep0359::StanzaId::new(
            "message-a".to_owned(),
            archive.clone().into(),
        ),
        by: archive.clone(),
        archive,
        archived_at: chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
        ordinal,
    }
}

#[test]
fn fresh_plan_restores_recorded_position_before_full_equality() {
    let saved = authority(Some(ArchiveOrdinal::from_storage(7).unwrap()));
    let mut planned = vec![authority(None)];
    restore(&mut planned, std::slice::from_ref(&saved)).unwrap();
    assert_eq!(planned, vec![saved]);
}

#[test]
fn conflicting_recorded_positions_are_non_advancing() {
    let first = ArchiveOrdinal::from_storage(7).unwrap();
    let second = ArchiveOrdinal::from_storage(8).unwrap();
    let saved = vec![authority(Some(first)), authority(Some(second))];
    let mut planned = vec![authority(None)];
    let error = restore(&mut planned, &saved).unwrap_err();
    assert!(matches!(
        error,
        IngressUowError::ArchiveOrdinalConflict { .. }
    ));
    assert_eq!(
        classify_failure(&error),
        IngressDecisionClass::IntentContradiction
    );
    assert_eq!(planned, vec![authority(None)]);
}

#[test]
fn recorded_and_planned_known_positions_cannot_be_overwritten() {
    let first = ArchiveOrdinal::from_storage(7).unwrap();
    let second = ArchiveOrdinal::from_storage(8).unwrap();
    let mut planned = vec![authority(Some(first))];
    assert!(matches!(
        restore(&mut planned, &[authority(Some(second))]),
        Err(IngressUowError::ArchiveOrdinalConflict { .. })
    ));
    assert_eq!(planned, vec![authority(Some(first))]);
}

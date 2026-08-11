use proptest::prelude::*;
use waddle_xmpp::ingress::{
    ConnectionGeneration, EntityGeneration, IngressOrdinal, IngressTypeError, NormalizedTarget,
    ProtocolEpoch, RowRevision,
};

#[test]
fn first_ordinal_is_one() {
    assert_eq!(IngressOrdinal::FIRST.to_storage(), 1);
    assert_eq!(
        IngressOrdinal::FIRST,
        IngressOrdinal::from_storage(1).expect("one must be a valid ingress ordinal")
    );
}

#[test]
fn zero_ordinal_is_invalid_from_storage() {
    assert!(IngressOrdinal::from_storage(0).is_err());
}

#[test]
fn ordinals_advance_monotonically_without_wrapping() {
    let first = IngressOrdinal::FIRST;
    let second = first.next().expect("first ordinal must have a successor");
    let third = second.next().expect("second ordinal must have a successor");

    assert!(first.to_storage() < second.to_storage());
    assert!(second.to_storage() < third.to_storage());
}

#[test]
fn ordinal_exhaustion_is_checked() {
    let maximum = IngressOrdinal::from_storage(u64::MAX)
        .expect("u64::MAX must be a valid final ingress ordinal");

    assert_eq!(maximum.next(), None);
}

proptest! {
    #[test]
    fn wire_h_matches_the_low_32_bits(value in 1u64..=u64::MAX) {
        let ordinal = IngressOrdinal::from_storage(value)
            .expect("proptest strategy only produces valid ingress ordinals");

        prop_assert_eq!(ordinal.wire_h(), value as u32);
    }
}

#[test]
fn generation_epoch_and_revision_counters_are_checked() {
    let entity = EntityGeneration::INITIAL;
    assert_eq!(entity.to_storage(), 0);
    assert_eq!(
        entity
            .next()
            .expect("initial entity generation advances")
            .to_storage(),
        1
    );
    assert_eq!(EntityGeneration::from_storage(u64::MAX).next(), None);

    let connection = ConnectionGeneration::INITIAL;
    assert_eq!(connection.to_storage(), 0);
    assert_eq!(
        connection
            .next()
            .expect("initial connection generation advances")
            .to_storage(),
        1
    );
    assert_eq!(ConnectionGeneration::from_storage(u64::MAX).next(), None);

    let revision = RowRevision::INITIAL;
    assert_eq!(revision.to_storage(), 0);
    assert_eq!(
        revision
            .next()
            .expect("initial row revision advances")
            .to_storage(),
        1
    );
    assert_eq!(RowRevision::from_storage(u64::MAX).next(), None);

    let epoch = ProtocolEpoch::ZERO;
    assert_eq!(epoch.to_storage(), 0);
    assert_eq!(
        epoch
            .next()
            .expect("zero protocol epoch advances")
            .to_storage(),
        1
    );
    assert_eq!(ProtocolEpoch::from_storage(u32::MAX).next(), None);
}

#[test]
fn normalized_target_storage_round_trips_all_target_shapes() {
    let absent = NormalizedTarget::Absent;
    let absent_storage = absent.to_storage();
    assert_eq!((absent_storage.kind(), absent_storage.jid()), (0, ""));
    assert_eq!(NormalizedTarget::from_storage(0, ""), Ok(absent));

    let bare = NormalizedTarget::Bare(
        "romeo@example.com"
            .parse()
            .expect("fixture is a valid bare JID"),
    );
    let bare_storage = bare.to_storage();
    assert_eq!(
        NormalizedTarget::from_storage(bare_storage.kind(), bare_storage.jid()),
        Ok(bare)
    );

    let full = NormalizedTarget::Full(
        "romeo@example.com/phone"
            .parse()
            .expect("fixture is a valid full JID"),
    );
    let full_storage = full.to_storage();
    assert_eq!(
        NormalizedTarget::from_storage(full_storage.kind(), full_storage.jid()),
        Ok(full)
    );
}

#[test]
fn normalized_target_storage_rejects_unknown_or_malformed_values() {
    assert_eq!(
        NormalizedTarget::from_storage(3, "romeo@example.com"),
        Err(IngressTypeError::InvalidNormalizedTargetStorage { kind: 3 })
    );
    assert_eq!(
        NormalizedTarget::from_storage(0, "romeo@example.com"),
        Err(IngressTypeError::InvalidNormalizedTargetStorage { kind: 0 })
    );
    assert_eq!(
        NormalizedTarget::from_storage(1, "romeo@example.com/phone"),
        Err(IngressTypeError::InvalidNormalizedTargetStorage { kind: 1 })
    );
    assert_eq!(
        NormalizedTarget::from_storage(2, "not a jid"),
        Err(IngressTypeError::InvalidNormalizedTargetStorage { kind: 2 })
    );
}

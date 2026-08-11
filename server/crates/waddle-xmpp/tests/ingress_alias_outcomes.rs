use proptest::prelude::*;
use uuid::Uuid;
use waddle_xmpp::ingress::{
    resolve_alias, AliasConflict, AliasOutcome, AliasResolution, MessageKey, SemanticDigest,
    StoredAlias,
};

fn digest(bytes: [u8; 32]) -> SemanticDigest {
    SemanticDigest::from_storage(1, bytes).expect("version one must be accepted")
}

fn key(value: u128) -> MessageKey {
    MessageKey::from_storage(Uuid::from_u128(value))
}

#[test]
fn no_origin_always_mints_a_fresh_key() {
    let offered = digest([1; 32]);
    let stored = StoredAlias {
        key: key(1),
        digest: digest([2; 32]),
    };
    let minted = key(2);

    assert_eq!(
        resolve_alias(false, &offered, Some(&stored), || minted),
        AliasResolution::NoOrigin(minted)
    );
}

#[test]
fn origin_without_stored_alias_inserts_a_minted_key() {
    let offered = digest([1; 32]);
    let minted = key(3);

    assert_eq!(
        resolve_alias(true, &offered, None, || minted),
        AliasResolution::Aliased(AliasOutcome::Inserted(minted))
    );
}

#[test]
fn equal_stored_digest_reuses_the_existing_key_without_minting() {
    let offered = digest([1; 32]);
    let stored = StoredAlias {
        key: key(4),
        digest: offered.clone(),
    };

    assert_eq!(
        resolve_alias(true, &offered, Some(&stored), || {
            panic!("an equal stored alias must not mint a key")
        }),
        AliasResolution::Aliased(AliasOutcome::Existing(stored.key))
    );
}

#[test]
fn different_stored_digest_returns_a_conflict() {
    let offered = digest([1; 32]);
    let stored = StoredAlias {
        key: key(5),
        digest: digest([2; 32]),
    };

    assert_eq!(
        resolve_alias(true, &offered, Some(&stored), || {
            panic!("a conflicting stored alias must not mint a key")
        }),
        AliasResolution::Aliased(AliasOutcome::Conflict(AliasConflict {
            existing: stored.key,
            stored: stored.digest.clone(),
            offered,
        }))
    );
}

#[test]
fn semantic_digest_rejects_an_unknown_version() {
    // V1 is the only real version in this shell, so a different-version
    // conflict cannot be constructed without inventing an unsupported variant.
    assert!(SemanticDigest::from_storage(2, [1; 32]).is_err());
}

proptest! {
    #[test]
    fn alias_resolution_is_deterministic(
        origin_present in any::<bool>(),
        offered_bytes in any::<[u8; 32]>(),
        stored_bytes in any::<[u8; 32]>(),
    ) {
        let offered = digest(offered_bytes);
        let stored = StoredAlias {
            key: key(6),
            digest: digest(stored_bytes),
        };
        let minted = key(7);

        let first = resolve_alias(origin_present, &offered, Some(&stored), || minted);
        let second = resolve_alias(origin_present, &offered, Some(&stored), || minted);

        prop_assert_eq!(first, second);
    }
}

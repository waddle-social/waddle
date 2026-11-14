//! Hard ceilings of the S2S certificate verifier.
//!
//! Every count and size is checked before the corresponding metadata is
//! parsed into owned values, so the work one certificate chain or one trust
//! profile can cause is bounded by these constants and not by the peer.
//! They are ceilings, not measured capacity claims; raising one needs the
//! review recorded in `docs/planning/s2s-verifier-2047.md`.

/// Certificates in one presented chain, the end-entity certificate included.
pub const MAX_CHAIN_CERTIFICATES: usize = 8;

/// Total DER bytes of one presented chain.
pub const MAX_CHAIN_DER_BYTES: usize = 64 * 1024;

/// DER bytes of one certificate, presented or configured.
pub const MAX_CERTIFICATE_DER_BYTES: usize = 16 * 1024;

/// Extensions in one certificate.
pub const MAX_CERTIFICATE_EXTENSIONS: usize = 32;

/// Entries in one subjectAltName extension.
pub const MAX_SUBJECT_ALT_NAMES: usize = 256;

/// Subtrees in one nameConstraints extension, permitted and excluded
/// together.
pub const MAX_NAME_CONSTRAINT_SUBTREES: usize = 64;

/// CA certificates in one private-CA profile, trust anchors and
/// intermediates together.
pub const MAX_PROFILE_CA_CERTIFICATES: usize = 8;

/// CRLs in one private-CA profile.
pub const MAX_PROFILE_CRLS: usize = 8;

/// DER bytes of one CRL.
pub const MAX_CRL_DER_BYTES: usize = 1024 * 1024;

/// Revoked-certificate entries in one CRL.
pub const MAX_CRL_ENTRIES: usize = 8_192;

/// Seconds in a day, the unit of the validity ceilings below.
pub const DAY_SECS: i64 = 86_400;

/// Longest interval from a CRL's thisUpdate to its nextUpdate on a
/// private-CA profile: seven days. A profile is usable until its earliest
/// nextUpdate, so this bounds how long a revocation the operator has not
/// yet loaded, or an old CRL loaded in place of a newer one, can go
/// unnoticed. An issuer that publishes CRLs valid for longer cannot be
/// used on a private-CA profile.
pub const MAX_CRL_VALIDITY_SECS: i64 = 7 * DAY_SECS;

/// Trust anchors in a substituted public root set.
pub const MAX_PUBLIC_ROOTS: usize = 512;

/// One step of [`PUBLIC_LEAF_VALIDITY_SCHEDULE`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeafValidityStep {
    /// The earliest notBefore the step applies to, in seconds since the
    /// Unix epoch.
    pub not_before_from: i64,
    /// The longest notAfter minus notBefore the step accepts, in seconds.
    pub max_validity_secs: i64,
}

/// Longest validity period of an end-entity certificate on the
/// public-WebPKI profile, by its notBefore, in ascending order.
///
/// The steps are those of CA/Browser Forum ballot SC-081, which amends the
/// TLS Baseline Requirements: 398 days for certificates issued before
/// 2026-03-15, 200 days from 2026-03-15 and 100 days from 2027-03-15. The
/// verifier reads the step dates as 00:00 UTC. The ballot schedules a
/// further step to 47 days from 2029-03-15, which this schedule does not
/// enforce. The schedule is re-verified against the Baseline Requirements
/// in force when federation is activated (ADR-0020 gate G22).
///
/// A publicly trusted certificate never exceeds its step, so the ceiling
/// refuses only misissued certificates. Public chains are not checked for
/// revocation; what the ceiling bounds is how long a certificate whose key
/// is later compromised can be used on this profile.
pub const PUBLIC_LEAF_VALIDITY_SCHEDULE: [LeafValidityStep; 3] = [
    LeafValidityStep {
        not_before_from: i64::MIN,
        max_validity_secs: 398 * DAY_SECS,
    },
    // 2026-03-15T00:00:00Z.
    LeafValidityStep {
        not_before_from: 1_773_532_800,
        max_validity_secs: 200 * DAY_SECS,
    },
    // 2027-03-15T00:00:00Z.
    LeafValidityStep {
        not_before_from: 1_805_068_800,
        max_validity_secs: 100 * DAY_SECS,
    },
];

/// The longest validity period, in seconds, of a public-WebPKI end-entity
/// certificate whose notBefore is `not_before` seconds after the Unix
/// epoch.
pub fn max_public_leaf_validity_secs(not_before: i64) -> i64 {
    PUBLIC_LEAF_VALIDITY_SCHEDULE
        .iter()
        .rev()
        .find(|step| not_before >= step.not_before_from)
        .map_or(PUBLIC_LEAF_VALIDITY_SCHEDULE[0].max_validity_secs, |step| {
            step.max_validity_secs
        })
}

/// Handshake jobs that may execute at once on the verifier workers.
pub const VERIFIER_WORKERS: usize = 4;

/// Callers that may wait for a verifier worker at once.
pub const VERIFIER_WAITERS: usize = 16;

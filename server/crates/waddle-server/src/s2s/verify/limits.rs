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

/// Trust anchors in a substituted public root set.
pub const MAX_PUBLIC_ROOTS: usize = 512;

/// Longest validity period of an end-entity certificate on the
/// public-WebPKI profile: 398 days, the ceiling the CA/Browser Forum
/// Baseline Requirements have imposed on publicly trusted TLS certificates
/// since September 2020. Public chains are not checked for revocation, so
/// this bounds how long a compromised certificate stays usable.
pub const MAX_PUBLIC_LEAF_VALIDITY_SECS: i64 = 398 * 86_400;

/// Handshake jobs that may execute at once on the verifier workers.
pub const VERIFIER_WORKERS: usize = 4;

/// Callers that may wait for a verifier worker at once.
pub const VERIFIER_WAITERS: usize = 16;

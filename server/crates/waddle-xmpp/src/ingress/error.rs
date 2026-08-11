use thiserror::Error;

/// Validation errors at the ingress storage boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum IngressTypeError {
    /// A durable ingress ordinal must start at one.
    #[error("zero is not a valid IngressOrdinal")]
    ZeroIngressOrdinal,
    /// The persisted digest-version discriminator is unsupported.
    #[error("unsupported ingress digest version: {value}")]
    UnsupportedDigestVersion { value: u8 },
    /// A persisted target discriminator is unknown or its JID is malformed.
    #[error("invalid normalized target storage representation for kind {kind}")]
    InvalidNormalizedTargetStorage { kind: i32 },
}

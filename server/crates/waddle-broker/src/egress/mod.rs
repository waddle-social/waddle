//! Constrained broker provider egress: approved `(provider, deployment, model,
//! operation)` profiles, DNS-resolved SSRF IP blocklist, TLS identity pinning,
//! `.no_proxy()`, no redirects, byte/time/concurrency/spend caps, and header/
//! error-body stripping.

pub mod client;
pub mod profile;
pub mod ssrf;

pub use client::{
    verify_dispatch_matches_request, BrokerEgressClient, EgressChunk, EgressError,
    EgressInferenceRequest, EgressResponse, EgressStream, EgressStreamEvent,
    ExpectedEgressBinding, ProviderOperationWire,
};
pub use profile::{
    is_blocked_admin_path, validate_operation_path, EgressBindingPolicy, EgressCaps,
    EgressProfileRegistry, ProfileError, ProviderOperation, ProviderProfile,
};
pub use ssrf::{resolve_and_validate_addrs, DnsResolver, SsrfError, TokioDnsResolver};

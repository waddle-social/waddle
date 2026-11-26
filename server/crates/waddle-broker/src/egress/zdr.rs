//! Qualified zero-data-retention (ZDR) and no-training route selection.
//!
//! Admins may select only routes whose exact `(provider, tier, deployment,
//! model, operation, downstream_routing)` tuple has verified `zdr == true`,
//! `data_collection == Deny`, `!allow_fallback`, and unexpired qualification
//! (`now_ms < qualified_until_ms`). Personal member keys that cannot establish
//! verified ZDR/no-training terms are ineligible and fail closed.

use std::collections::BTreeMap;

use thiserror::Error;

use crate::egress::profile::ProviderOperation;

/// Upstream provider data-collection policy attached to a route qualification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DataCollectionPolicy {
    Deny,
    Allow,
}

/// Origin of the credential backing a candidate route selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RouteKeyOrigin {
    /// Broker-managed tenant credential covered by an enterprise ZDR/no-training DPA.
    ManagedTenant,
    /// Personal member-funded key: eligible ONLY when `verified_zdr_agreement == true`.
    PersonalMember { verified_zdr_agreement: bool },
}

/// An admin-selectable provider route with explicit ZDR and no-training attestation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualifiedZdrRoute {
    pub provider: String,
    pub tier: String,
    pub deployment: String,
    pub model: String,
    pub operation: ProviderOperation,
    pub downstream_routing: String,
    pub data_collection: DataCollectionPolicy,
    pub zdr: bool,
    pub allow_fallback: bool,
    pub qualified_until_ms: i64,
}

/// Query describing an exact candidate route and caller key provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZdrRouteQuery<'a> {
    pub provider: &'a str,
    pub tier: &'a str,
    pub deployment: &'a str,
    pub model: &'a str,
    pub operation: ProviderOperation,
    pub downstream_routing: &'a str,
    pub key_origin: RouteKeyOrigin,
    pub now_ms: i64,
}

/// Fail-closed errors returned when registering or selecting a ZDR route.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RouteQualificationError {
    #[error("route field {0} must be non-empty")]
    EmptyField(&'static str),
    #[error("route does not enable zero-data-retention (zdr=true required)")]
    ZdrNotEnabled,
    #[error("route does not enforce data_collection=deny")]
    DataCollectionNotDenied,
    #[error("route fallback to unqualified downstream providers is prohibited")]
    FallbackProhibited,
    #[error("personal key cannot establish verified ZDR/no-training terms")]
    PersonalKeyIneligible,
    #[error("route qualification is unknown or not approved")]
    UnknownRoute,
    #[error(
        "route qualification expired (qualified_until_ms={qualified_until_ms}, now_ms={now_ms})"
    )]
    QualificationExpired {
        qualified_until_ms: i64,
        now_ms: i64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ZdrRouteKey {
    provider: String,
    tier: String,
    deployment: String,
    model: String,
    operation: ProviderOperation,
    downstream_routing: String,
}

/// Fail-closed registry of qualified `(provider, tier, deployment, model,
/// operation, downstream_routing)` routes.
#[derive(Debug, Clone, Default)]
pub struct ZdrRouteRegistry {
    routes: BTreeMap<ZdrRouteKey, QualifiedZdrRoute>,
}

impl ZdrRouteRegistry {
    /// Construct an empty fail-closed ZDR route registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Validate and register a qualified ZDR/no-training route.
    ///
    /// # Errors
    ///
    /// Rejects empty fields, `zdr == false`, `data_collection != Deny`,
    /// `allow_fallback == true`, or non-positive qualification expiry.
    pub fn register_qualified(
        &mut self,
        route: QualifiedZdrRoute,
    ) -> Result<(), RouteQualificationError> {
        validate_route(&route)?;
        let key = ZdrRouteKey {
            provider: route.provider.clone(),
            tier: route.tier.clone(),
            deployment: route.deployment.clone(),
            model: route.model.clone(),
            operation: route.operation,
            downstream_routing: route.downstream_routing.clone(),
        };
        self.routes.insert(key, route);
        Ok(())
    }

    /// Select a route only when exact tuple matching, ZDR, `data_collection=deny`,
    /// no-fallback, key provenance, and strict expiry (`now_ms < qualified_until_ms`)
    /// all pass.
    ///
    /// # Errors
    ///
    /// Fails closed on personal keys lacking ZDR agreements, unknown routes,
    /// expired qualification, or any weakened ZDR/no-training policy.
    pub fn select_route(
        &self,
        query: &ZdrRouteQuery<'_>,
    ) -> Result<&QualifiedZdrRoute, RouteQualificationError> {
        if let RouteKeyOrigin::PersonalMember {
            verified_zdr_agreement: false,
        } = query.key_origin
        {
            return Err(RouteQualificationError::PersonalKeyIneligible);
        }
        validate_non_empty("provider", query.provider)?;
        validate_non_empty("tier", query.tier)?;
        validate_non_empty("deployment", query.deployment)?;
        validate_non_empty("model", query.model)?;
        validate_non_empty("downstream_routing", query.downstream_routing)?;

        let key = ZdrRouteKey {
            provider: query.provider.to_owned(),
            tier: query.tier.to_owned(),
            deployment: query.deployment.to_owned(),
            model: query.model.to_owned(),
            operation: query.operation,
            downstream_routing: query.downstream_routing.to_owned(),
        };
        let route = self
            .routes
            .get(&key)
            .ok_or(RouteQualificationError::UnknownRoute)?;
        validate_route(route)?;
        if query.now_ms >= route.qualified_until_ms {
            return Err(RouteQualificationError::QualificationExpired {
                qualified_until_ms: route.qualified_until_ms,
                now_ms: query.now_ms,
            });
        }
        Ok(route)
    }
}

fn validate_non_empty(field: &'static str, value: &str) -> Result<(), RouteQualificationError> {
    if value.trim().is_empty() {
        return Err(RouteQualificationError::EmptyField(field));
    }
    Ok(())
}

fn validate_route(route: &QualifiedZdrRoute) -> Result<(), RouteQualificationError> {
    validate_non_empty("provider", &route.provider)?;
    validate_non_empty("tier", &route.tier)?;
    validate_non_empty("deployment", &route.deployment)?;
    validate_non_empty("model", &route.model)?;
    validate_non_empty("downstream_routing", &route.downstream_routing)?;
    if !route.zdr {
        return Err(RouteQualificationError::ZdrNotEnabled);
    }
    if route.data_collection != DataCollectionPolicy::Deny {
        return Err(RouteQualificationError::DataCollectionNotDenied);
    }
    if route.allow_fallback {
        return Err(RouteQualificationError::FallbackProhibited);
    }
    if route.qualified_until_ms <= 0 {
        return Err(RouteQualificationError::QualificationExpired {
            qualified_until_ms: route.qualified_until_ms,
            now_ms: 0,
        });
    }
    Ok(())
}

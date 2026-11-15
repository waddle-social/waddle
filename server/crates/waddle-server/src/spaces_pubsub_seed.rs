//! Seed `Affiliation::Owner` rows on Spaces PubSub nodes for entities that
//! administer Spaces via the Zanzibar permission model (server owners,
//! space creators).
//!
//! XEP-0060 §4.1 puts authorization in the affiliation table; this module
//! mirrors the Zanzibar permission decisions into durable affiliation rows
//! so that `pubsub_authz::can_administer` (which only consults storage) can
//! make the right call for `<configure/>`, `<purge/>`, and `<affiliations/>`
//! operations against the Spaces service.
//!
//! A seed is a bootstrap and repair writer, so it never lifts a ban and
//! never grants outside the authority it is given ([`OwnerSeedPolicy`]):
//!
//! - An existing `Outcast` row is kept. The write and the check are one
//!   step in the store (`PubSubStorage::set_affiliation_unless_outcast`), so
//!   a ban recorded concurrently is not overwritten either. Only an explicit
//!   affiliation change lifts a ban.
//! - Owner is a privileged grant: it is written only for an account the
//!   policy's issuance input admits to one, an account of a served domain or
//!   of a trusted home (ADR-0020 D8).
//! - The Space node of an enrolled host-grant scope is never seeded: its
//!   authority is the canonical store, and a node whose enrollment cannot be
//!   read is skipped as well.

use std::sync::Arc;

use jid::BareJid;
use tracing::warn;
use waddle_xmpp::pubsub::{Affiliation, PubSubStorage};
use waddle_xmpp::XmppError;

use crate::permissions::host_grant::{HostGrantGate, IssuancePolicy};

/// What a seed may write.
#[derive(Clone, Copy)]
pub struct OwnerSeedPolicy<'a> {
    /// The served account domains and the trusted homes a privileged grant
    /// may name.
    pub issuance: &'a IssuancePolicy,
    /// The enrolled host-grant scopes whose Space nodes are never seeded.
    pub host_grants: &'a HostGrantGate,
}

/// What one seed of one entity on one node did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnerSeedOutcome {
    /// The entity holds `Owner` on the node.
    Seeded,
    /// The entity is an outcast of the node and stays one.
    KeptOutcast,
    /// The policy admits no privileged grant for the entity.
    RefusedSubject,
    /// The node belongs to an enrolled host-grant scope, or whether it does
    /// could not be read.
    RefusedNode,
    /// The store failed; the failure was logged.
    Failed,
}

/// Write `Affiliation::Owner` for each entity in `entities` against
/// `(spaces_jid, node)`, under `policy`. Idempotent on re-run.
///
/// Per-entity write failures are logged with `warn!` and do not abort the
/// batch.
pub async fn seed_owners_on_node(
    storage: &Arc<dyn PubSubStorage>,
    spaces_jid: &BareJid,
    node: &str,
    entities: &[BareJid],
    policy: OwnerSeedPolicy<'_>,
) -> Vec<OwnerSeedOutcome> {
    if entities.is_empty() {
        return Vec::new();
    }
    if !node_may_be_seeded(policy, node).await {
        return vec![OwnerSeedOutcome::RefusedNode; entities.len()];
    }
    let mut outcomes = Vec::with_capacity(entities.len());
    for entity in entities {
        outcomes.push(seed_owner_one(storage, spaces_jid, node, entity, policy).await);
    }
    outcomes
}

/// Write `Affiliation::Owner` for `entity` against every node owned by
/// `spaces_jid`, under `policy`. Used at startup to mirror server-owner
/// permissions across all existing Spaces nodes.
///
/// Per-node write failures are logged with `warn!` and do not abort the
/// batch. A failure to enumerate nodes propagates as `Err` because the
/// caller cannot proceed without the node list.
pub async fn seed_owner_on_all_nodes(
    storage: &Arc<dyn PubSubStorage>,
    spaces_jid: &BareJid,
    entity: &BareJid,
    policy: OwnerSeedPolicy<'_>,
) -> Result<Vec<OwnerSeedOutcome>, XmppError> {
    let nodes = storage.list_nodes(spaces_jid).await?;
    let mut outcomes = Vec::with_capacity(nodes.len());
    for node in &nodes {
        outcomes.push(if node_may_be_seeded(policy, node).await {
            seed_owner_one(storage, spaces_jid, node, entity, policy).await
        } else {
            OwnerSeedOutcome::RefusedNode
        });
    }
    Ok(outcomes)
}

/// The issuance policy of a seed on a deployment whose accounts live at
/// `account_domain`: privileged seeds name accounts of that domain, and no
/// trusted home is configured for bootstrap and repair writers (ADR-0020
/// D8). `None` when the domain is not a canonical account domain.
pub fn account_domain_seed_issuance(account_domain: &str) -> Option<IssuancePolicy> {
    let domain = waddle_xmpp::identity::CanonicalDomain::parse_exact(account_domain).ok()?;
    IssuancePolicy::new([domain], [], []).ok()
}

async fn node_may_be_seeded(policy: OwnerSeedPolicy<'_>, node: &str) -> bool {
    policy.host_grants.space_enrolled(node).await == Ok(false)
}

async fn seed_owner_one(
    storage: &Arc<dyn PubSubStorage>,
    spaces_jid: &BareJid,
    node: &str,
    entity: &BareJid,
    policy: OwnerSeedPolicy<'_>,
) -> OwnerSeedOutcome {
    if !policy.issuance.admits_privileged_subject(entity) {
        warn!(
            kind = "subject_refused",
            "refused to seed an Owner affiliation for an account outside the served domains and \
             trusted homes",
        );
        return OwnerSeedOutcome::RefusedSubject;
    }
    match storage
        .set_affiliation_unless_outcast(spaces_jid, node, entity, Affiliation::Owner)
        .await
    {
        Ok(true) => OwnerSeedOutcome::Seeded,
        Ok(false) => OwnerSeedOutcome::KeptOutcast,
        Err(error) => {
            warn!(
                spaces = %spaces_jid,
                node = %node,
                entity = %entity,
                error = %error,
                "failed to seed Owner affiliation on Spaces node",
            );
            OwnerSeedOutcome::Failed
        }
    }
}

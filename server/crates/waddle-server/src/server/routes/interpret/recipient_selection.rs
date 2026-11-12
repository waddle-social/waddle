//! Freeze original and carbon destinations before recipient preparation.
use super::{BareJid, Deps, FullJid, Jid};
use waddle_xmpp::registry::user_actor::ResourceRoutingState;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct RecipientInventory {
    pub live: Vec<ResourceRoutingState>,
    pub detached: Vec<FullJid>,
    pub detached_carbons: Vec<FullJid>,
    pub detached_headlines: Vec<FullJid>,
}

pub(super) struct RecipientSelection {
    pub originals: Vec<FullJid>,
    pub carbons: Vec<FullJid>,
}

impl RecipientInventory {
    /// RFC 6121 §8.5.2.1.1: headline originals reach every available
    /// non-negative resource; an unmatched full JID never falls back to bare.
    pub(super) fn select_headline(&self, requested: &Jid) -> RecipientSelection {
        let mut originals = match requested.try_as_full() {
            Ok(full) => {
                if self.live.iter().any(|resource| &resource.jid == full)
                    || self.detached.contains(full)
                {
                    vec![full.clone()]
                } else {
                    Vec::new()
                }
            }
            Err(_) => self
                .live
                .iter()
                .filter(|resource| resource.available && resource.priority >= 0)
                .map(|resource| resource.jid.clone())
                .chain(self.detached_headlines.iter().cloned())
                .collect(),
        };
        originals.sort();
        originals.dedup();
        RecipientSelection {
            originals,
            carbons: Vec::new(),
        }
    }
    pub(super) fn select(&self, requested: &Jid) -> RecipientSelection {
        let full = requested.clone().try_into_full().ok();
        let mut originals = match full.filter(|full| {
            self.live.iter().any(|resource| &resource.jid == full) || self.detached.contains(full)
        }) {
            Some(full) => vec![full],
            None => {
                let priority = self
                    .live
                    .iter()
                    .filter(|resource| resource.available && resource.priority >= 0)
                    .map(|resource| resource.priority)
                    .max();
                let mut originals: Vec<_> = self
                    .live
                    .iter()
                    .filter(|resource| match priority {
                        Some(priority) => resource.available && resource.priority == priority,
                        None => !resource.available,
                    })
                    .map(|resource| resource.jid.clone())
                    .collect();
                originals.extend(self.detached.iter().cloned());
                originals
            }
        };
        originals.sort();
        originals.dedup();
        let mut carbons: Vec<_> = self
            .live
            .iter()
            .filter(|resource| resource.carbons_enabled)
            .map(|resource| resource.jid.clone())
            .chain(self.detached_carbons.iter().cloned())
            .filter(|jid| !originals.contains(jid))
            .collect();
        carbons.sort();
        carbons.dedup();
        RecipientSelection { originals, carbons }
    }
}

/// Every connected resource of `bare`, read from the selected account
/// authority. A coordinator that does not answer yields no resource, like
/// an actor tree that does not answer; the registry is not read instead.
pub(super) async fn live_resources(deps: &Deps<'_>, bare: &BareJid) -> Vec<FullJid> {
    if let Some(state) = deps.web_socket_state {
        if let Some(resources) = state
            .deps
            .app_state
            .account_authority()
            .resources(bare)
            .await
        {
            return resources.unwrap_or_else(|error| {
                tracing::warn!(%error, "account coordinator did not answer a resource read");
                Vec::new()
            });
        }
    }
    match deps.user_registry {
        Some(registry) => waddle_xmpp::registry::get_resources_for_user(registry, bare).await,
        None => Vec::new(),
    }
}

pub(crate) async fn local_recipient_inventory(
    deps: &Deps<'_>,
    bare: &BareJid,
) -> Result<RecipientInventory, ()> {
    let selected = match deps.web_socket_state {
        Some(state) => {
            state
                .deps
                .app_state
                .account_authority()
                .routing_resources(bare)
                .await
        }
        None => None,
    };
    let live = match selected {
        // The coordinator is the selected authority. Its failure is a
        // failed read; the registry is not written in this mode and must
        // not answer instead.
        Some(resources) => resources.map_err(|_| ())?,
        None => match deps.user_registry {
            Some(registry) => waddle_xmpp::registry::routing_resources_for_user(registry, bare)
                .await
                .map_err(|_| ())?,
            None => Vec::new(),
        },
    };
    let (detached, detached_carbons, detached_headlines) = match deps.sm_session_registry {
        Some(sm) => (
            sm.detached_resources_for_user(bare).await.map_err(|_| ())?,
            sm.detached_carbon_resources_for_user(bare, &[])
                .await
                .map_err(|_| ())?,
            sm.available_detached_presence_states_for_user(bare)
                .await
                .map_err(|_| ())?
                .into_iter()
                .filter(|state| state.priority >= 0)
                .map(|state| state.resource)
                .collect(),
        ),
        None => (Vec::new(), Vec::new(), Vec::new()),
    };
    Ok(RecipientInventory {
        live,
        detached,
        detached_carbons,
        detached_headlines,
    })
}

pub(super) async fn recipient_inventory(
    deps: &Deps<'_>,
    bare: &BareJid,
) -> Result<RecipientInventory, ()> {
    #[cfg(feature = "clustering")]
    if let Some(addressing) = deps
        .web_socket_state
        .and_then(|state| state.deps.app_state.clustering_claims.addressing())
    {
        if let Some(inventory) = addressing.account(bare).recipient_inventory().await? {
            return Ok(inventory);
        }
    }
    local_recipient_inventory(deps, bare).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resource(name: &str, priority: i8) -> ResourceRoutingState {
        ResourceRoutingState {
            jid: format!("bob@example.com/{name}")
                .parse()
                .expect("valid recipient resource JID"),
            available: true,
            priority,
            carbons_enabled: true,
        }
    }

    #[test]
    fn archived_headlines_freeze_all_available_nonnegative_resources_without_full_fallback() {
        let low = resource("low", 1);
        let high = resource("high", 10);
        let negative = resource("negative", -1);
        let mut unavailable = resource("unavailable", 20);
        unavailable.available = false;
        let inventory = RecipientInventory {
            live: vec![low.clone(), high.clone(), negative.clone(), unavailable],
            ..Default::default()
        };
        let selected = inventory
            .select_headline(&"bob@example.com".parse().expect("valid bare recipient JID"));
        assert_eq!(selected.originals, vec![high.jid, low.jid]);
        assert!(selected.carbons.is_empty());
        assert!(inventory
            .select_headline(
                &"bob@example.com/missing"
                    .parse()
                    .expect("valid missing recipient resource JID")
            )
            .originals
            .is_empty());
        assert_eq!(
            inventory
                .select_headline(&negative.jid.clone().into())
                .originals,
            vec![negative.jid]
        );
    }

    #[test]
    fn full_hit_excludes_only_addressed_original_and_missing_full_uses_bare_priority() {
        let phone = resource("phone", 1);
        let desktop = resource("desktop", 2);
        let inventory = RecipientInventory {
            live: vec![phone.clone(), desktop.clone()],
            ..Default::default()
        };
        let full = inventory.select(&phone.jid.clone().into());
        assert_eq!(full.originals, vec![phone.jid.clone()]);
        assert_eq!(full.carbons, vec![desktop.jid.clone()]);
        let fallback = inventory.select(
            &"bob@example.com/gone"
                .parse()
                .expect("valid unavailable recipient resource JID"),
        );
        assert_eq!(fallback.originals, vec![desktop.jid]);
        assert_eq!(fallback.carbons, vec![phone.jid]);
    }

    #[test]
    fn detached_originals_are_excluded_from_received_carbons() {
        let phone = resource("phone", 1);
        let inventory = RecipientInventory {
            detached: vec![phone.jid.clone()],
            detached_carbons: vec![phone.jid.clone()],
            ..Default::default()
        };
        let selection =
            inventory.select(&"bob@example.com".parse().expect("valid bare recipient JID"));
        assert_eq!(selection.originals, vec![phone.jid]);
        assert!(selection.carbons.is_empty());
    }
}

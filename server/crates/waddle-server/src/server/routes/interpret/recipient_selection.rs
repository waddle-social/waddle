//! Freeze original and carbon destinations before recipient preparation.
use super::*;
use waddle_xmpp::registry::user_actor::ResourceRoutingState;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct RecipientInventory {
    pub live: Vec<ResourceRoutingState>,
    pub detached: Vec<FullJid>,
    pub detached_carbons: Vec<FullJid>,
}

pub(super) struct RecipientSelection {
    pub originals: Vec<FullJid>,
    pub carbons: Vec<FullJid>,
}

impl RecipientInventory {
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

pub(crate) async fn local_recipient_inventory(
    deps: &Deps<'_>,
    bare: &BareJid,
) -> Result<RecipientInventory, ()> {
    let live = match deps.user_registry {
        Some(registry) => waddle_xmpp::registry::routing_resources_for_user(registry, bare)
            .await
            .map_err(|_| ())?,
        None => Vec::new(),
    };
    let (detached, detached_carbons) = match deps.sm_session_registry {
        Some(sm) => (
            sm.detached_resources_for_user(bare).await.map_err(|_| ())?,
            sm.detached_carbon_resources_for_user(bare, &[])
                .await
                .map_err(|_| ())?,
        ),
        None => (Vec::new(), Vec::new()),
    };
    Ok(RecipientInventory {
        live,
        detached,
        detached_carbons,
    })
}

pub(super) async fn recipient_inventory(
    deps: &Deps<'_>,
    bare: &BareJid,
) -> Result<RecipientInventory, ()> {
    #[cfg(feature = "clustering")]
    if let Some(bridge) = deps.web_socket_state.and_then(|state| {
        state
            .deps
            .app_state
            .clustering_claims
            .ordered_relay_delivery_bridge
            .as_ref()
    }) {
        if let Some(inventory) = bridge.recipient_inventory_remote(bare).await? {
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
            jid: format!("bob@example.com/{name}").parse().unwrap(),
            available: true,
            priority,
            carbons_enabled: true,
        }
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
        let fallback = inventory.select(&"bob@example.com/gone".parse().unwrap());
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
        let selection = inventory.select(&"bob@example.com".parse().unwrap());
        assert_eq!(selection.originals, vec![phone.jid]);
        assert!(selection.carbons.is_empty());
    }
}

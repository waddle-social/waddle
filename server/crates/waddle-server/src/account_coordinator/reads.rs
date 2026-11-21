//! Account reads that routing takes from the account coordinator.
//!
//! Each read is the answer of the coordinator on the node that owns the
//! account, or its error. An application that never selected the
//! coordinator answers [`AccountCoordinatorError::Unselected`]. The
//! connection registry and the `UserActor` are not account authorities and
//! a caller must not read them in place of an error.

use jid::{BareJid, FullJid};
use waddle_xmpp::ownership::account_coordinator::AccountResourcePresence;
use waddle_xmpp::ownership::BindingRef;
use waddle_xmpp::registry::user_actor::ResourceRoutingState;

use super::authority::AccountBindingAuthority;
use super::config::AccountCoordinatorConfig;
use super::error::AccountCoordinatorError;
use super::messages::AccountView;
use super::selection::AccountAuthoritySelection;

impl AccountAuthoritySelection {
    async fn coordinator_view(
        &self,
        account: &BareJid,
    ) -> Result<AccountView, AccountCoordinatorError> {
        match self.authority() {
            Some(authority) => authority.view(account).await,
            None => Err(AccountCoordinatorError::Unselected),
        }
    }

    /// Select one connected headline destination, or verify the exact resource
    /// chosen before a relay hop, from a single authoritative account view.
    pub(crate) async fn headline_binding(
        &self,
        account: &BareJid,
        selected: Option<&FullJid>,
    ) -> Result<Option<(FullJid, BindingRef)>, AccountCoordinatorError> {
        select_headline_binding(&self.coordinator_view(account).await?, selected)
    }

    /// Read every eligible headline binding from one coordinator snapshot.
    /// Higher priority sorts first, with full JID as the tie-breaker.
    pub(crate) async fn headline_bindings(
        &self,
        account: &BareJid,
    ) -> Result<Vec<(FullJid, BindingRef)>, AccountCoordinatorError> {
        select_headline_bindings(&self.coordinator_view(account).await?, None)
    }

    /// Availability, priority and carbons of every active resource, in the
    /// shape recipient selection freezes.
    pub(crate) async fn routing_resources(
        &self,
        account: &BareJid,
    ) -> Result<Vec<ResourceRoutingState>, AccountCoordinatorError> {
        self.coordinator_view(account).await.map(|view| {
            view.routing
                .into_iter()
                .map(|resource| ResourceRoutingState {
                    jid: resource.jid,
                    available: resource.available,
                    priority: resource.priority,
                    carbons_enabled: resource.carbons_enabled,
                })
                .collect()
        })
    }

    /// The live destinations of a stanza addressed to the bare JID: the
    /// routable resources, or, when no resource is available with a
    /// non-negative priority, every active resource that has not announced
    /// a negative priority.
    pub(crate) async fn bare_jid_targets(
        &self,
        account: &BareJid,
    ) -> Result<Vec<FullJid>, AccountCoordinatorError> {
        self.coordinator_view(account).await.map(|view| {
            if !view.routable.is_empty() {
                return view.routable;
            }
            view.routing
                .into_iter()
                .filter(|resource| !(resource.available && resource.priority < 0))
                .map(|resource| resource.jid)
                .collect()
        })
    }

    /// The active binding of one resource: the only binding delivery may
    /// hand a stanza for it to. `Ok(None)` means the resource has no active
    /// binding.
    pub(crate) async fn bound_resource(
        &self,
        resource: &FullJid,
    ) -> Result<Option<BindingRef>, AccountCoordinatorError> {
        self.coordinator_view(&resource.to_bare())
            .await
            .map(|view| view.binding(resource).cloned())
    }

    /// Every active resource, with no presence filter.
    pub(crate) async fn resources(
        &self,
        account: &BareJid,
    ) -> Result<Vec<FullJid>, AccountCoordinatorError> {
        self.coordinator_view(account)
            .await
            .map(|view| view.resources)
    }

    /// Every presence-available resource with its priority.
    pub(crate) async fn available_resources(
        &self,
        account: &BareJid,
    ) -> Result<Vec<(FullJid, i8)>, AccountCoordinatorError> {
        self.coordinator_view(account)
            .await
            .map(|view| view.available)
    }

    /// Every presence-available resource paired with its last announced
    /// presence state from one coherent coordinator view.
    pub(crate) async fn available_resources_with_presence(
        &self,
        account: &BareJid,
    ) -> Result<Vec<(FullJid, Option<AccountResourcePresence>)>, AccountCoordinatorError> {
        self.coordinator_view(account).await.map(|view| {
            let mut presences = view.presence;
            view.available
                .into_iter()
                .map(|(jid, _priority)| {
                    let state = presences
                        .iter()
                        .position(|presence| presence.jid == jid)
                        .map(|index| presences.swap_remove(index));
                    (jid, state)
                })
                .collect()
        })
    }

    /// The presence an available resource last announced. `Ok(None)` means
    /// the resource is not available.
    #[cfg(test)]
    pub(crate) async fn presence(
        &self,
        resource: &FullJid,
    ) -> Result<Option<AccountResourcePresence>, AccountCoordinatorError> {
        self.coordinator_view(&resource.to_bare())
            .await
            .map(|view| {
                view.presence
                    .into_iter()
                    .find(|presence| &presence.jid == resource)
            })
    }

    /// The active resources with carbons enabled, other than `exclude`.
    pub(crate) async fn carbon_resources(
        &self,
        account: &BareJid,
        exclude: &[FullJid],
    ) -> Result<Vec<FullJid>, AccountCoordinatorError> {
        self.coordinator_view(account).await.map(|view| {
            view.routing
                .into_iter()
                .filter(|resource| resource.carbons_enabled && !exclude.contains(&resource.jid))
                .map(|resource| resource.jid)
                .collect()
        })
    }

    /// The active resources that want roster pushes.
    pub(crate) async fn roster_interested(
        &self,
        account: &BareJid,
    ) -> Result<Vec<FullJid>, AccountCoordinatorError> {
        self.coordinator_view(account)
            .await
            .map(|view| view.roster_interested)
    }

    /// The active resources that want block-list pushes.
    pub(crate) async fn blocklist_interested(
        &self,
        account: &BareJid,
    ) -> Result<Vec<FullJid>, AccountCoordinatorError> {
        self.coordinator_view(account)
            .await
            .map(|view| view.blocklist_interested)
    }
}

fn select_headline_binding(
    view: &AccountView,
    selected: Option<&FullJid>,
) -> Result<Option<(FullJid, BindingRef)>, AccountCoordinatorError> {
    Ok(select_headline_bindings(view, selected)?.into_iter().next())
}

fn select_headline_bindings(
    view: &AccountView,
    selected: Option<&FullJid>,
) -> Result<Vec<(FullJid, BindingRef)>, AccountCoordinatorError> {
    let max_resources = AccountCoordinatorConfig::default().max_resources.get();
    if view.routing.len() > max_resources || view.bindings.len() > max_resources {
        return Err(AccountCoordinatorError::Diverged);
    }
    let mut resources: Vec<_> = view
        .routing
        .iter()
        .filter(|resource| {
            resource.available
                && resource.priority >= 0
                && selected.is_none_or(|jid| &resource.jid == jid)
        })
        .collect();
    resources.sort_unstable_by(|left, right| {
        right
            .priority
            .cmp(&left.priority)
            .then_with(|| left.jid.cmp(&right.jid))
    });
    let mut seen = std::collections::BTreeSet::new();
    resources
        .into_iter()
        .map(|resource| {
            if !seen.insert(&resource.jid) {
                return Err(AccountCoordinatorError::Diverged);
            }
            let binding = view
                .binding(&resource.jid)
                .ok_or(AccountCoordinatorError::Diverged)?;
            Ok((resource.jid.clone(), binding.clone()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headline_selects_only_an_available_exact_binding() {
        use waddle_xmpp::ownership::account_coordinator::AccountResourceRouting;
        use waddle_xmpp::ownership::{ConnectionRef, NodeLeaseIdentity};

        let resource = |name, available, priority| AccountResourceRouting {
            jid: format!("alice@example.com/{name}")
                .parse()
                .expect("resource"),
            available,
            priority,
            carbons_enabled: false,
        };
        let phone = resource("phone", true, 0);
        let desktop = resource("desktop", true, 5);
        let laptop = resource("laptop", true, 5);
        let away = resource("away", false, 127);
        let negative = resource("negative", true, -1);
        let binding =
            || BindingRef::fresh(ConnectionRef::fresh(NodeLeaseIdentity::local_process()));
        let phone_binding = binding();
        let desktop_binding = binding();
        let laptop_binding = binding();
        let mut view = AccountView {
            routing: vec![
                phone.clone(),
                laptop.clone(),
                desktop.clone(),
                away.clone(),
                negative.clone(),
            ],
            bindings: vec![
                (phone.jid.clone(), phone_binding.clone()),
                (desktop.jid.clone(), desktop_binding.clone()),
                (laptop.jid.clone(), laptop_binding.clone()),
                (away.jid.clone(), binding()),
                (negative.jid.clone(), binding()),
            ],
            ..AccountView::default()
        };

        assert_eq!(
            select_headline_bindings(&view, None).expect("all eligible bindings"),
            [
                (desktop.jid.clone(), desktop_binding.clone()),
                (laptop.jid, laptop_binding),
                (phone.jid.clone(), phone_binding.clone()),
            ]
        );
        assert_eq!(
            select_headline_binding(&view, None).expect("authoritative selection"),
            Some((desktop.jid.clone(), desktop_binding))
        );
        assert_eq!(
            select_headline_binding(&view, Some(&phone.jid)).expect("exact selection"),
            Some((phone.jid, phone_binding))
        );
        assert!(select_headline_binding(&view, Some(&negative.jid))
            .expect("negative priority")
            .is_none());
        view.bindings.retain(|(jid, _)| jid != &desktop.jid);
        assert!(matches!(
            select_headline_binding(&view, None),
            Err(AccountCoordinatorError::Diverged)
        ));
        view.routing = vec![away, negative];
        assert!(select_headline_binding(&view, None)
            .expect("no available resource")
            .is_none());
    }

    #[test]
    fn headline_inventory_rejects_an_oversized_view() {
        use waddle_xmpp::ownership::account_coordinator::AccountResourceRouting;

        let view = AccountView {
            routing: vec![
                AccountResourceRouting {
                    jid: "alice@example.com/negative".parse().expect("resource"),
                    available: true,
                    priority: -1,
                    carbons_enabled: false,
                };
                AccountCoordinatorConfig::default().max_resources.get() + 1
            ],
            ..AccountView::default()
        };
        assert!(matches!(
            select_headline_bindings(&view, None),
            Err(AccountCoordinatorError::Diverged)
        ));
    }

    /// An application that never ran startup's selection has no account
    /// authority: every read fails instead of answering from another
    /// source.
    #[tokio::test]
    async fn unselected_authority_answers_no_read() {
        let selection = AccountAuthoritySelection::Unselected;
        let account: BareJid = "alice@example.com".parse().expect("bare jid");
        let resource: FullJid = "alice@example.com/phone".parse().expect("full jid");
        assert!(matches!(
            selection.resources(&account).await,
            Err(AccountCoordinatorError::Unselected)
        ));
        assert!(matches!(
            selection.headline_bindings(&account).await,
            Err(AccountCoordinatorError::Unselected)
        ));
        assert!(matches!(
            selection.headline_binding(&account, None).await,
            Err(AccountCoordinatorError::Unselected)
        ));
        assert!(matches!(
            selection.routing_resources(&account).await,
            Err(AccountCoordinatorError::Unselected)
        ));
        assert!(matches!(
            selection.bare_jid_targets(&account).await,
            Err(AccountCoordinatorError::Unselected)
        ));
        assert!(matches!(
            selection.bound_resource(&resource).await,
            Err(AccountCoordinatorError::Unselected)
        ));
        assert!(matches!(
            selection.presence(&resource).await,
            Err(AccountCoordinatorError::Unselected)
        ));
        assert!(matches!(
            selection.carbon_resources(&account, &[]).await,
            Err(AccountCoordinatorError::Unselected)
        ));
        assert!(matches!(
            selection.blocklist_interested(&account).await,
            Err(AccountCoordinatorError::Unselected)
        ));
        assert!(selection.coordinators().is_none());
        assert!(selection.authority().is_none());
    }
}

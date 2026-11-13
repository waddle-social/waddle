//! Account reads that routing takes from the selected authority.
//!
//! Each read returns `None` when the connection registry is the selected
//! authority, and the caller then reads the registry as it always has. When
//! the coordinator is selected the read is the answer of the coordinator on
//! the node that owns the account, or its error; a caller must not replace
//! an error with a registry read, because the registry is not written in
//! that mode.

use jid::{BareJid, FullJid};
use waddle_xmpp::ownership::account_coordinator::AccountResourcePresence;
use waddle_xmpp::ownership::BindingRef;
use waddle_xmpp::registry::user_actor::ResourceRoutingState;

use super::authority::AccountBindingAuthority;
use super::error::AccountCoordinatorError;
use super::messages::AccountView;
use super::selection::AccountAuthoritySelection;

impl AccountAuthoritySelection {
    async fn coordinator_view(
        &self,
        account: &BareJid,
    ) -> Option<Result<AccountView, AccountCoordinatorError>> {
        match self.authority() {
            Some(authority) => Some(authority.view(account).await),
            None => None,
        }
    }

    /// Availability, priority and carbons of every active resource, in the
    /// shape recipient selection freezes.
    pub(crate) async fn routing_resources(
        &self,
        account: &BareJid,
    ) -> Option<Result<Vec<ResourceRoutingState>, AccountCoordinatorError>> {
        Some(self.coordinator_view(account).await?.map(|view| {
            view.routing
                .into_iter()
                .map(|resource| ResourceRoutingState {
                    jid: resource.jid,
                    available: resource.available,
                    priority: resource.priority,
                    carbons_enabled: resource.carbons_enabled,
                })
                .collect()
        }))
    }

    /// The live destinations of a stanza addressed to the bare JID: the
    /// routable resources, or, when no resource is available with a
    /// non-negative priority, every active resource that has not announced
    /// a negative priority.
    pub(crate) async fn bare_jid_targets(
        &self,
        account: &BareJid,
    ) -> Option<Result<Vec<FullJid>, AccountCoordinatorError>> {
        Some(self.coordinator_view(account).await?.map(|view| {
            if !view.routable.is_empty() {
                return view.routable;
            }
            view.routing
                .into_iter()
                .filter(|resource| !(resource.available && resource.priority < 0))
                .map(|resource| resource.jid)
                .collect()
        }))
    }

    /// The active binding of one resource: the only binding delivery may
    /// hand a stanza for it to. `Ok(None)` means the resource has no active
    /// binding.
    pub(crate) async fn bound_resource(
        &self,
        resource: &FullJid,
    ) -> Option<Result<Option<BindingRef>, AccountCoordinatorError>> {
        Some(
            self.coordinator_view(&resource.to_bare())
                .await?
                .map(|view| view.binding(resource).cloned()),
        )
    }

    /// Every active resource, with no presence filter.
    pub(crate) async fn resources(
        &self,
        account: &BareJid,
    ) -> Option<Result<Vec<FullJid>, AccountCoordinatorError>> {
        Some(
            self.coordinator_view(account)
                .await?
                .map(|view| view.resources),
        )
    }

    /// Every presence-available resource with its priority.
    pub(crate) async fn available_resources(
        &self,
        account: &BareJid,
    ) -> Option<Result<Vec<(FullJid, i8)>, AccountCoordinatorError>> {
        Some(
            self.coordinator_view(account)
                .await?
                .map(|view| view.available),
        )
    }

    /// The presence an available resource last announced. `Ok(None)` means
    /// the resource is not available.
    pub(crate) async fn presence(
        &self,
        resource: &FullJid,
    ) -> Option<Result<Option<AccountResourcePresence>, AccountCoordinatorError>> {
        Some(
            self.coordinator_view(&resource.to_bare())
                .await?
                .map(|view| {
                    view.presence
                        .into_iter()
                        .find(|presence| &presence.jid == resource)
                }),
        )
    }

    /// The active resources with carbons enabled, other than `exclude`.
    pub(crate) async fn carbon_resources(
        &self,
        account: &BareJid,
        exclude: &[FullJid],
    ) -> Option<Result<Vec<FullJid>, AccountCoordinatorError>> {
        Some(self.coordinator_view(account).await?.map(|view| {
            view.routing
                .into_iter()
                .filter(|resource| resource.carbons_enabled && !exclude.contains(&resource.jid))
                .map(|resource| resource.jid)
                .collect()
        }))
    }

    /// The active resources that want roster pushes.
    pub(crate) async fn roster_interested(
        &self,
        account: &BareJid,
    ) -> Option<Result<Vec<FullJid>, AccountCoordinatorError>> {
        Some(
            self.coordinator_view(account)
                .await?
                .map(|view| view.roster_interested),
        )
    }

    /// The active resources that want block-list pushes.
    pub(crate) async fn blocklist_interested(
        &self,
        account: &BareJid,
    ) -> Option<Result<Vec<FullJid>, AccountCoordinatorError>> {
        Some(
            self.coordinator_view(account)
                .await?
                .map(|view| view.blocklist_interested),
        )
    }
}

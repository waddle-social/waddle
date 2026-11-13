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

    /// The presence an available resource last announced. `Ok(None)` means
    /// the resource is not available.
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

#[cfg(test)]
mod tests {
    use super::*;

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

//! Reconcile durable extension authority before accepting extension invocations.

use crate::ingress_uow::{
    ConfiguredPluginGrants, ExtensionGrantRepository, GrantSync, IngressUnitOfWork, IngressUowError,
};
use waddle_extensions::{ExtensionCapability, ExtensionManager};

pub(crate) async fn sync_extension_grants(
    manager: &ExtensionManager,
    uow: &IngressUnitOfWork,
) -> Result<GrantSync, IngressUowError> {
    let configured: Vec<_> = manager
        .configured_plugins()
        .map(|(manifest, provider_rooms)| ConfiguredPluginGrants {
            can_send: manifest.declares_capability(ExtensionCapability::HostMessageSend),
            plugin: manifest.id,
            provider_rooms,
        })
        .collect();
    let mut tx = uow.begin().await?;
    let result = ExtensionGrantRepository::sync_configured(&mut tx, &configured).await?;
    tx.commit().await?;
    Ok(result)
}

#[cfg(test)]
mod tests;

//! Extension bots: automated XMPP entities at `<plugin>@<extensions domain>/bot`.
//!
//! A bot has no account; the server speaks for it. It is identified the
//! way XMPP intends: XEP-0030 identity `client/bot` for the entity,
//! the server-assigned XEP-0317 Bot hat in rooms, and XEP-0045
//! affiliation/role for authority only.

use std::sync::Arc;

use jid::{BareJid, Jid};
use waddle_extensions::{ExtensionManager, ExtensionManifest, PluginId};
use waddle_xmpp::xep::xep0317::{well_known, Hat, HatSet, ServerHats};

use crate::server::routes::websocket::XmppServiceDomains;

/// The one resource every extension bot binds.
pub(crate) const RESOURCE: &str = "bot";

impl XmppServiceDomains {
    /// Any address on the extensions service domain. Nothing there accepts
    /// messages or presence subscriptions.
    pub(crate) fn is_extensions_address(&self, jid: &Jid) -> bool {
        jid.domain().as_str() == self.extensions
    }

    /// The plugin whose bot `jid` addresses: its bare JID or `/bot`.
    pub(crate) fn extension_bot(&self, jid: &Jid) -> Option<PluginId> {
        if !self.is_extensions_address(jid) {
            return None;
        }
        if jid
            .resource()
            .is_some_and(|resource| resource.as_str() != RESOURCE)
        {
            return None;
        }
        PluginId::new(jid.node()?.as_str()).ok()
    }

    /// The bot's own full JID.
    pub(crate) fn extension_bot_jid(&self, plugin: &PluginId) -> Result<jid::FullJid, jid::Error> {
        BareJid::new(&format!("{}@{}", plugin.as_str(), self.extensions))?
            .with_resource_str(RESOURCE)
    }
}

/// Room nick and disco identity name: profile display name, else manifest name, else id.
pub(crate) fn bot_name(manager: &ExtensionManager, plugin: &PluginId) -> String {
    manifest_bot_name(
        manager.manifest_for_plugin(plugin.as_str()).as_ref(),
        plugin,
    )
}

fn manifest_bot_name(manifest: Option<&ExtensionManifest>, plugin: &PluginId) -> String {
    manifest
        .map(|manifest| {
            manifest
                .profile
                .as_ref()
                .map(|profile| profile.display_name.as_str())
                .unwrap_or_else(|| manifest.name.as_str())
                .trim()
                .to_string()
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| plugin.as_str().to_string())
}

/// An extension bot that exists: its plugin is installed on this node.
pub(crate) struct InstalledBot {
    pub name: String,
    /// The manifest profile's description.
    pub description: Option<String>,
}

/// The installed bot `jid` addresses (bare JID or `/bot`), if any. Every
/// answer the server gives on a bot's behalf starts here.
pub(crate) fn installed_bot(
    domains: &XmppServiceDomains,
    manager: &ExtensionManager,
    jid: &Jid,
) -> Option<InstalledBot> {
    let plugin = domains.extension_bot(jid)?;
    let manifest = manager.manifest_for_plugin(plugin.as_str())?;
    Some(InstalledBot {
        name: manifest_bot_name(Some(&manifest), &plugin),
        description: manifest
            .profile
            .and_then(|profile| profile.description)
            .map(|description| description.as_str().trim().to_string())
            .filter(|description| !description.is_empty()),
    })
}

/// Give every extension bot the Bot hat, titled by its manifest label. A bot
/// whose manifest is missing on this node still wears the default hat.
pub(crate) fn install_bot_hats(
    hats: &ServerHats,
    domains: XmppServiceDomains,
    manager: Arc<ExtensionManager>,
) {
    hats.install(move |occupant: &BareJid| {
        let plugin = domains.extension_bot(&Jid::from(occupant.clone()))?;
        let hat = manager
            .manifest_for_plugin(plugin.as_str())
            .and_then(|manifest| manifest.profile?.bot_hat_label)
            .map(|label| Hat::new(label.as_str(), well_known::BOT))
            .unwrap_or_else(Hat::bot);
        Some(HatSet::new().with_hat(hat))
    });
}

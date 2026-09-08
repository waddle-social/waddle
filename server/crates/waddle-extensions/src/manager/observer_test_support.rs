use super::*;
use crate::observer_test_support::ObserverTestPlugin;

impl ExtensionManager {
    /// Load real fixture actors with independent deterministic invocation controls.
    pub async fn with_observer_test_plugins(plugins: Vec<Arc<ObserverTestPlugin>>) -> Self {
        let runtime = WasmRuntime::new().expect("fixture runtime");
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/message_hook.wat");
        let mut actors = Vec::new();
        for plugin in plugins {
            let loaded = LoadedExtension::load(&runtime, &path).expect("fixture component");
            let actor = WasmExtensionActor::initialize(loaded, "1")
                .await
                .expect("fixture observer")
                .with_grants(HashSet::from([ExtensionCapability::MessageObserve]))
                .with_observer_test(plugin);
            actors.push(Arc::new(actor));
        }
        Self {
            actors,
            feature_namespaces: Vec::new(),
            route_descriptors: Vec::new(),
            launch_signing_key: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observer_test_support::ObserverTestBehavior;

    fn message() -> Message {
        let mut message = Message::new(Some("room@muc.example.com".parse().expect("room")));
        message.from = Some("alice@example.com/web".parse().expect("sender"));
        message.bodies.insert(
            xmpp_parsers::message::Lang(String::new()),
            "committed body".to_owned(),
        );
        message
    }

    #[tokio::test]
    async fn observer_manager_selects_and_invokes_each_plugin_independently() {
        let success_id = PluginId::new("observer-success").expect("plugin");
        let warning_id = PluginId::new("observer-warning").expect("plugin");
        let success = ObserverTestPlugin::new(success_id.clone(), ObserverTestBehavior::Success);
        let warning = ObserverTestPlugin::new(warning_id.clone(), ObserverTestBehavior::Warning);
        let manager =
            ExtensionManager::with_observer_test_plugins(vec![success.clone(), warning.clone()])
                .await;
        let message = message();
        assert_eq!(
            manager.message_observer_plugins(&message),
            [success_id.clone(), warning_id.clone()]
        );
        let waddle = WaddleId::new("space").expect("waddle");
        let result = manager
            .process_message_observer(&success_id, &message, waddle.clone(), None)
            .await
            .expect("selected");
        assert!(result.effects.is_empty());
        assert!(warning.invocations().is_empty());
        let result = manager
            .process_message_observer(&warning_id, &message, waddle, None)
            .await
            .expect("selected");
        assert!(matches!(
            result.effects.as_slice(),
            [ExtensionEffect::HostWarning(_)]
        ));
        assert_eq!(success.invocations()[0].body.as_str(), "committed body");
        assert_eq!(warning.invocations()[0].body.as_str(), "committed body");
    }

    #[tokio::test]
    async fn observer_manager_rechecks_grants_and_message_eligibility() {
        let id = PluginId::new("observer-revoked").expect("plugin");
        let plugin = ObserverTestPlugin::new(id.clone(), ObserverTestBehavior::Success);
        let manager = ExtensionManager::with_observer_test_plugins(vec![plugin.clone()]).await;
        let mut message = message();
        assert_eq!(
            manager.message_observer_plugins(&message),
            std::slice::from_ref(&id)
        );
        let missing = PluginId::new("observer-missing").expect("plugin");
        assert!(manager
            .process_message_observer(
                &missing,
                &message,
                WaddleId::new("space").expect("waddle"),
                None
            )
            .await
            .is_none());
        message.bodies.clear();
        assert!(manager.message_observer_plugins(&message).is_empty());
        assert!(manager
            .process_message_observer(&id, &message, WaddleId::new("space").expect("waddle"), None)
            .await
            .is_none());
        message.bodies.insert(
            xmpp_parsers::message::Lang(String::new()),
            "committed body".to_owned(),
        );
        plugin.revoke();
        assert!(manager.message_observer_plugins(&message).is_empty());
        assert!(manager
            .process_message_observer(&id, &message, WaddleId::new("space").expect("waddle"), None)
            .await
            .is_none());
        assert!(plugin.invocations().is_empty());
    }
}

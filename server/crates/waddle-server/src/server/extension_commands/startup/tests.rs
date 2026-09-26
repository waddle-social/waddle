use super::*;
use crate::ingress::test_support::IngressFixture;
use crate::ingress_uow::{ConfiguredPluginGrants, ExtensionGrantRepository};
use waddle_extensions::{ExtensionCapability, ExtensionConfig, ExtensionModuleConfig, PluginId};

fn config(can_send: bool, rooms: &[&str]) -> ExtensionConfig {
    let capability = if can_send {
        ExtensionCapability::HostMessageSend
    } else {
        ExtensionCapability::MessageEnrich
    };
    ExtensionConfig {
        modules: vec![ExtensionModuleConfig {
            name: "message-hook-fixture".into(),
            registry: String::new(),
            digest: None,
            tag: None,
            namespace: "urn:test:message-hook".into(),
            // The fixture exposes a single capability selected by its WIT enum index.
            config: serde_json::json!(if can_send { 8 } else { 0 }),
            capability_grants: vec![capability, ExtensionCapability::HostMessageSend],
            allowed_http_origins: Vec::new(),
            provider_room_grants: rooms.iter().map(|room| (*room).to_owned()).collect(),
            config_secret_files: Default::default(),
            local_path: Some(
                std::path::PathBuf::from(
                    std::env::var_os("CARGO_MANIFEST_DIR")
                        .expect("test runner sets CARGO_MANIFEST_DIR"),
                )
                .join("../waddle-extensions/tests/fixtures/message_hook.wasm")
                .to_string_lossy()
                .into_owned(),
            ),
        }],
        ..ExtensionConfig::default()
    }
}

async fn startup_sync(fixture: IngressFixture) {
    let plugin = PluginId::new("message-hook-fixture").expect("plugin");
    let removed = PluginId::new("removed-plugin").expect("removed plugin");
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
    let old_room: jid::BareJid = "old@muc.example.com".parse().expect("old room");
    let mut tx = fixture.uow.begin().await.expect("seed transaction");
    ExtensionGrantRepository::sync_configured(
        &mut tx,
        &[
            ConfiguredPluginGrants {
                plugin: removed.clone(),
                can_send: true,
                provider_rooms: vec![],
            },
            ConfiguredPluginGrants {
                plugin: plugin.clone(),
                can_send: true,
                provider_rooms: vec![old_room.clone()],
            },
        ],
    )
    .await
    .expect("seed grants");
    tx.commit().await.expect("commit seed");

    let manager = ExtensionManager::from_config(config(true, &["room@muc.example.com"]))
        .await
        .expect("loaded manager");
    let result = sync_extension_grants(&manager, &fixture.uow)
        .await
        .expect("sync startup");
    assert_eq!(
        result,
        GrantSync {
            inserted: 1,
            revoked: 2
        }
    );
    let mut tx = fixture.uow.begin().await.expect("read grants");
    let send = ExtensionGrantRepository::active_send_grant(&mut tx, &plugin)
        .await
        .expect("send lookup");
    assert!(send.is_some());
    assert!(
        ExtensionGrantRepository::active_room_grant(&mut tx, &plugin, &room)
            .await
            .expect("room lookup")
            .is_some()
    );
    assert!(
        ExtensionGrantRepository::active_room_grant(&mut tx, &plugin, &old_room)
            .await
            .expect("old room lookup")
            .is_none()
    );
    assert!(
        ExtensionGrantRepository::active_send_grant(&mut tx, &removed)
            .await
            .expect("removed lookup")
            .is_none()
    );
    tx.commit().await.expect("read commit");
    assert_eq!(
        sync_extension_grants(&manager, &fixture.uow)
            .await
            .expect("idempotent sync"),
        GrantSync::default()
    );

    let no_send = ExtensionManager::from_config(config(false, &["room@muc.example.com"]))
        .await
        .expect("manifest without send despite config grant");
    assert_eq!(
        sync_extension_grants(&no_send, &fixture.uow)
            .await
            .expect("capability loss"),
        GrantSync {
            inserted: 0,
            revoked: 2
        }
    );
    for enabled in [false, true] {
        sync_extension_grants(&manager, &fixture.uow)
            .await
            .expect("restore grants");
        let mut empty_config = config(true, &["room@muc.example.com"]);
        empty_config.enabled = enabled;
        if enabled {
            empty_config.modules.clear();
        }
        let empty = ExtensionManager::from_config(empty_config)
            .await
            .expect("disabled or empty manager");
        assert_eq!(
            sync_extension_grants(&empty, &fixture.uow)
                .await
                .expect("revoke all"),
            GrantSync {
                inserted: 0,
                revoked: 2
            }
        );
    }
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_startup_sync_uses_complete_loaded_configuration() {
    startup_sync(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_startup_sync_uses_complete_loaded_configuration() {
    if let Some(fixture) = IngressFixture::postgres("extension_startup_sync").await {
        startup_sync(fixture).await;
    }
}

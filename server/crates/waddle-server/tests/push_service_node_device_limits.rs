//! Push Service node/device registration validation and quota tests.
//!
//! Exercises `DatabasePushServiceStore` through its public API: node
//! reuse per `(owner, app-id)`, field-length validation, per-owner and
//! per-node quotas, and Web Push endpoint SSRF gating.

use jid::BareJid;
use minidom::Element;
use waddle_server::db::Database;
use waddle_server::push_service::{
    DatabasePushServiceStore, PushDevicePlatform, PushDeviceRegistration,
};
use waddle_xmpp::pubsub::PubSubItem;
use waddle_xmpp::xep::xep0357::NS_PUSH;
use waddle_xmpp::XmppError;

/// Mirrors `MAX_PUSH_NODES_PER_OWNER` in `waddle_server::push_service`.
const MAX_PUSH_NODES_PER_OWNER: i64 = 16;
/// Mirrors `MAX_PUSH_DEVICES_PER_NODE` in `waddle_server::push_service`.
const MAX_PUSH_DEVICES_PER_NODE: i64 = 32;
/// Mirrors `MAX_APP_ID_LEN` in `waddle_server::push_service`.
const MAX_APP_ID_LEN: usize = 128;
/// Mirrors `MAX_NODE_ID_LEN` in `waddle_server::push_service`.
const MAX_NODE_ID_LEN: usize = 256;
/// Mirrors `MAX_DEVICE_ID_LEN` in `waddle_server::push_service`.
const MAX_DEVICE_ID_LEN: usize = 128;
/// Mirrors `MAX_ENVIRONMENT_LEN` in `waddle_server::push_service`.
const MAX_ENVIRONMENT_LEN: usize = 64;
/// Mirrors `MAX_PROVIDER_ENDPOINT_LEN` in `waddle_server::push_service`.
const MAX_PROVIDER_ENDPOINT_LEN: usize = 2_048;
/// Mirrors `MAX_PROVIDER_TOKEN_LEN` in `waddle_server::push_service`.
const MAX_PROVIDER_TOKEN_LEN: usize = 4_096;
/// Mirrors `MAX_PROVIDER_KEY_MATERIAL_LEN` in `waddle_server::push_service`.
const MAX_PROVIDER_KEY_MATERIAL_LEN: usize = 4_096;
/// Mirrors `MAX_PUBSUB_ITEM_ID_LEN` in `waddle_server::push_service`.
const MAX_PUBSUB_ITEM_ID_LEN: usize = 256;

async fn store() -> DatabasePushServiceStore {
    DatabasePushServiceStore::new_with_secret_key(
        Database::in_memory("push-service")
            .await
            .expect("push service db"),
        b"waddle-push-service-test-secret-key",
    )
    .await
    .expect("push service store")
}

fn owner() -> BareJid {
    "alice@example.com".parse().expect("owner jid")
}

fn notification_item(item_id: &str) -> PubSubItem {
    PubSubItem::new(
        Some(item_id.to_string()),
        Some(Element::builder("notification", NS_PUSH).build()),
    )
}

fn assert_bad_request(error: XmppError) {
    assert!(matches!(
        error,
        XmppError::Stanza {
            condition: waddle_xmpp::StanzaErrorCondition::BadRequest,
            ..
        }
    ));
}

#[test]
fn push_device_registration_debug_redacts_provider_credentials() {
    let registration =
        PushDeviceRegistration::new("web-1", "push-node", PushDevicePlatform::Web, "prod")
            .with_provider_endpoint(Some(
                "https://updates.push.services.mozilla.com/secret-endpoint".to_string(),
            ))
            .with_provider_token(Some("provider-secret-token".to_string()))
            .with_provider_key_material(Some("provider-secret-key".to_string()));

    let debug = format!("{registration:?}");

    assert!(debug.contains("PushDeviceRegistration"));
    assert!(debug.contains("web-1"));
    assert!(debug.contains("<redacted>"));
    assert!(!debug.contains("secret-endpoint"));
    assert!(!debug.contains("provider-secret-token"));
    assert!(!debug.contains("provider-secret-key"));
}

#[tokio::test]
async fn ensure_node_reuses_one_node_per_user_app() {
    let store = store().await;
    let first = store
        .ensure_node(&owner(), "ios")
        .await
        .expect("first node");
    let second = store
        .ensure_node(&owner(), "ios")
        .await
        .expect("second node");

    assert_eq!(first.node(), second.node());
    assert_eq!(first.owner_bare_jid(), &owner());
    assert_eq!(first.app_id(), "ios");
    assert_eq!(
        store
            .list_node_names_for_owner(&owner())
            .await
            .expect("nodes")
            .len(),
        1
    );
}

#[tokio::test]
async fn web_push_endpoint_rejects_ssrf_vectors() {
    let store = store().await;
    let owner = owner();
    let node = store.ensure_node(&owner, "web").await.expect("node");
    for bad in [
        // Literal IPs in any family must be rejected outright —
        // legitimate relays use named hosts.
        "https://127.0.0.1/abc",
        "https://10.0.0.5/abc",
        "https://169.254.169.254/latest/meta-data/",
        "https://[::1]/abc",
        "https://[fd00::1]/abc",
        // localhost and any *.localhost subdomain.
        "https://localhost/abc",
        "https://foo.localhost/abc",
        // Non-https schemes.
        "http://push.example.com/abc",
        "ftp://push.example.com/abc",
    ] {
        let err = store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new("ssrf", node.node(), PushDevicePlatform::Web, "test")
                    .with_provider_endpoint(Some(bad.to_string())),
            )
            .await
            .expect_err(&format!("must reject {bad}"));
        match err {
            XmppError::Stanza {
                condition: waddle_xmpp::StanzaErrorCondition::BadRequest,
                ..
            } => {}
            other => panic!("expected bad-request for {bad}, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn web_push_endpoint_accepts_real_relay_hosts() {
    let store = store().await;
    let owner = owner();
    let node = store.ensure_node(&owner, "web").await.expect("node");
    for ok in [
        "https://fcm.googleapis.com/fcm/send/abc",
        "https://updates.push.services.mozilla.com/wpush/v1/abc",
        "https://web.push.apple.com/abc",
        // Self-hosted relays with named hosts are fine.
        "https://push.internal.example.com/abc",
        // Explicit default port `:443` must be accepted.
        "https://fcm.googleapis.com:443/fcm/send/abc",
        // Non-default ports are also allowed: RFC 8030 / RFC 8292
        // place no constraint on port, and the VAPID `aud` claim
        // is computed from the URL origin (scheme + host + port),
        // so self-hosted Mozilla autopush deployments on `:8443`
        // must round-trip through this gate cleanly.
        "https://autopush.self-hosted.example.com:8443/wpush/v1/abc",
    ] {
        store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new("good", node.node(), PushDevicePlatform::Web, "test")
                    .with_provider_endpoint(Some(ok.to_string())),
            )
            .await
            .unwrap_or_else(|err| panic!("legitimate relay {ok} rejected: {err:?}"));
    }
}

#[tokio::test]
async fn ensure_node_rejects_oversized_app_id() {
    let store = store().await;
    let err = store
        .ensure_node(&owner(), &"x".repeat(MAX_APP_ID_LEN + 1))
        .await
        .expect_err("oversized app-id rejected");

    assert_bad_request(err);
}

#[tokio::test]
async fn node_quota_limits_new_nodes_per_owner() {
    let store = store().await;
    let owner = owner();
    for idx in 0..MAX_PUSH_NODES_PER_OWNER {
        store
            .ensure_node(&owner, &format!("app-{idx}"))
            .await
            .expect("node within quota");
    }

    let err = store
        .ensure_node(&owner, "app-over-quota")
        .await
        .expect_err("node over quota rejected");

    assert_bad_request(err);
}

#[tokio::test]
async fn upsert_device_rejects_oversized_provider_token() {
    let store = store().await;
    let owner = owner();
    let node = store.ensure_node(&owner, "web").await.expect("push node");
    let err = store
        .upsert_device(
            &owner,
            PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test")
                .with_provider_token(Some("x".repeat(MAX_PROVIDER_TOKEN_LEN + 1))),
        )
        .await
        .expect_err("oversized token rejected");

    assert_bad_request(err);
}

#[tokio::test]
async fn upsert_device_rejects_oversized_device_registration_fields() {
    let store = store().await;
    let owner = owner();
    let node = store.ensure_node(&owner, "web").await.expect("push node");
    let cases = [
        PushDeviceRegistration::new(
            "x".repeat(MAX_DEVICE_ID_LEN + 1),
            node.node(),
            PushDevicePlatform::Web,
            "test",
        ),
        PushDeviceRegistration::new(
            "web-1",
            "x".repeat(MAX_NODE_ID_LEN + 1),
            PushDevicePlatform::Web,
            "test",
        ),
        PushDeviceRegistration::new(
            "web-1",
            node.node(),
            PushDevicePlatform::Web,
            "x".repeat(MAX_ENVIRONMENT_LEN + 1),
        ),
        PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test")
            .with_provider_endpoint(Some("x".repeat(MAX_PROVIDER_ENDPOINT_LEN + 1))),
        PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test")
            .with_provider_key_material(Some("x".repeat(MAX_PROVIDER_KEY_MATERIAL_LEN + 1))),
    ];

    for registration in cases {
        let err = store
            .upsert_device(&owner, registration)
            .await
            .expect_err("oversized registration field rejected");
        assert_bad_request(err);
    }
}

#[tokio::test]
async fn publish_notification_rejects_oversized_pubsub_item_id() {
    let store = store().await;
    let owner = owner();
    let node = store.ensure_node(&owner, "web").await.expect("push node");
    let err = store
        .publish_notification_from_user_server(
            node.node(),
            &notification_item(&"x".repeat(MAX_PUBSUB_ITEM_ID_LEN + 1)),
            &owner,
        )
        .await
        .expect_err("oversized item id rejected");

    assert_bad_request(err);
}

#[tokio::test]
async fn device_quota_limits_new_devices_per_node() {
    let store = store().await;
    let owner = owner();
    let node = store.ensure_node(&owner, "web").await.expect("push node");
    for idx in 0..MAX_PUSH_DEVICES_PER_NODE {
        store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new(
                    format!("web-{idx}"),
                    node.node(),
                    PushDevicePlatform::Web,
                    "test",
                ),
            )
            .await
            .expect("device within quota");
    }

    let err = store
        .upsert_device(
            &owner,
            PushDeviceRegistration::new(
                "web-over-quota",
                node.node(),
                PushDevicePlatform::Web,
                "test",
            ),
        )
        .await
        .expect_err("device over quota rejected");

    assert_bad_request(err);
}

#[tokio::test]
async fn device_quota_counts_active_devices_not_retired_devices() {
    let store = store().await;
    let owner = owner();
    let node = store.ensure_node(&owner, "web").await.expect("push node");
    for idx in 0..MAX_PUSH_DEVICES_PER_NODE {
        let device_id = format!("web-{idx}");
        store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new(
                    device_id.as_str(),
                    node.node(),
                    PushDevicePlatform::Web,
                    "test",
                )
                .with_provider_token(Some(format!("provider-secret-{idx}"))),
            )
            .await
            .expect("device within quota");
        store
            .disable_device_for_owner(&owner, node.node(), &device_id, None)
            .await
            .expect("disable retained device");
    }

    let fresh = store
        .upsert_device(
            &owner,
            PushDeviceRegistration::new(
                "web-after-retired-quota",
                node.node(),
                PushDevicePlatform::Web,
                "test",
            ),
        )
        .await
        .expect("retired disabled devices must not permanently exhaust active quota");

    assert_eq!(fresh.device_id(), "web-after-retired-quota");
}

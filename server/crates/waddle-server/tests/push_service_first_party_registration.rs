//! First-party XEP-0357 enable/registration interplay between the Push
//! Service store and the user-server `push_registrations` store.
//!
//! Covers transactional rollback on registration insert failure,
//! publish-options propagation into registrations and publish jobs, and
//! the server-origin PubSub IQ publish path requiring a live
//! registration.

use jid::BareJid;
use minidom::Element;
use waddle_server::db::{Database, IntoParams, Rows};
use waddle_server::db_params;
use waddle_server::push_registrations::DatabasePushRegistrationStore;
use waddle_server::push_service::{
    DatabasePushServiceStore, PushDevicePlatform, PushDeviceRegistration,
};
use waddle_xmpp::pubsub::PubSubItem;
use waddle_xmpp::push::PushSubscriptionStore;
use waddle_xmpp::xep::xep0357::NS_PUSH;
use waddle_xmpp::XmppError;
use xmpp_parsers::iq::Iq;

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

fn publish_options_with_field(var: &str, value: &str) -> Element {
    Element::builder("x", waddle_xmpp::xep::NS_DATA_FORMS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
        .append(
            Element::builder("field", waddle_xmpp::xep::NS_DATA_FORMS)
                .attr(minidom::rxml::xml_ncname!("var").to_owned(), "FORM_TYPE")
                .append(
                    Element::builder("value", waddle_xmpp::xep::NS_DATA_FORMS)
                        .append(waddle_xmpp::xep::NS_PUBSUB_PUBLISH_OPTIONS)
                        .build(),
                )
                .build(),
        )
        .append(
            Element::builder("field", waddle_xmpp::xep::NS_DATA_FORMS)
                .attr(minidom::rxml::xml_ncname!("var").to_owned(), var)
                .append(
                    Element::builder("value", waddle_xmpp::xep::NS_DATA_FORMS)
                        .append(value)
                        .build(),
                )
                .build(),
        )
        .build()
}

fn xep0357_pubsub_publish_iq(
    push_service_jid: &str,
    publisher: &BareJid,
    node: &str,
    item: &PubSubItem,
    publish_options: Option<&Element>,
) -> Iq {
    let publish = Element::builder("publish", waddle_xmpp::pubsub::NS_PUBSUB)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
        .append(item.to_element(waddle_xmpp::pubsub::NS_PUBSUB))
        .build();
    let mut pubsub = Element::builder("pubsub", waddle_xmpp::pubsub::NS_PUBSUB).append(publish);
    if let Some(publish_options) = publish_options {
        pubsub = pubsub.append(
            Element::builder("publish-options", waddle_xmpp::pubsub::NS_PUBSUB)
                .append(publish_options.clone())
                .build(),
        );
    }
    Iq::Set {
        from: Some(publisher.clone().into()),
        to: Some(push_service_jid.parse().expect("push service jid")),
        id: "push-publish-test".to_string(),
        payload: pubsub.build(),
    }
}

async fn execute(store: &DatabasePushServiceStore, sql: &str, params: impl IntoParams) {
    let db = store.database();
    let conn = db.guard().await.expect("db guard");
    conn.execute(sql, params).await.expect("execute");
}

async fn query(store: &DatabasePushServiceStore, sql: &str, params: impl IntoParams) -> Rows {
    let db = store.database();
    let conn = db.guard().await.expect("db guard");
    conn.query(sql, params).await.expect("query")
}

async fn scalar_i64(store: &DatabasePushServiceStore, sql: &str, params: impl IntoParams) -> i64 {
    let mut rows = query(store, sql, params).await;
    let row = rows.next().await.expect("scalar row").expect("scalar row");
    row.get(0).expect("scalar value")
}

fn assert_item_not_found(error: XmppError) {
    assert!(matches!(
        error,
        XmppError::Stanza {
            condition: waddle_xmpp::StanzaErrorCondition::ItemNotFound,
            ..
        }
    ));
}

#[tokio::test]
async fn first_party_enable_rolls_back_when_registration_insert_fails() {
    let store = store().await;
    let owner = owner();
    let node = store.ensure_node(&owner, "web").await.expect("push node");
    store
        .upsert_device(
            &owner,
            PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test")
                .with_provider_token(Some("provider-secret".to_string())),
        )
        .await
        .expect("device");
    let registration_store = DatabasePushRegistrationStore::new(store.database())
        .await
        .expect("registration store");
    let owner_lock_updated_before = scalar_i64(
        &store,
        "SELECT updated_at_ms FROM push_owner_locks WHERE owner_bare_jid = ?",
        db_params![owner.to_string()],
    )
    .await;
    let node_lock_updated_before = scalar_i64(
        &store,
        "SELECT updated_at_ms FROM push_node_locks WHERE node = ?",
        db_params![node.node()],
    )
    .await;
    execute(
        &store,
        r#"
        CREATE TRIGGER fail_push_registration_insert
        BEFORE INSERT ON push_registrations
        BEGIN
            SELECT RAISE(ABORT, 'forced push registration insert failure');
        END
        "#,
        (),
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;

    store
        .register_first_party_node_for_owner(&owner, "push.example.com", node.node(), None)
        .await
        .expect_err("forced insert failure should abort first-party enable transaction");

    let registrations = registration_store
        .get_for_user(&owner.to_string())
        .await
        .expect("registrations");
    let owner_lock_updated_after = scalar_i64(
        &store,
        "SELECT updated_at_ms FROM push_owner_locks WHERE owner_bare_jid = ?",
        db_params![owner.to_string()],
    )
    .await;
    let node_lock_updated_after = scalar_i64(
        &store,
        "SELECT updated_at_ms FROM push_node_locks WHERE node = ?",
        db_params![node.node()],
    )
    .await;
    let publish = store
        .publish_notification_from_user_server(
            node.node(),
            &notification_item("after-enable-rollback"),
            &owner,
        )
        .await
        .expect("push node remains usable after rollback");

    assert!(registrations.is_empty());
    assert_eq!(owner_lock_updated_after, owner_lock_updated_before);
    assert_eq!(node_lock_updated_after, node_lock_updated_before);
    assert_eq!(publish.attempted_devices(), 1);
}

#[tokio::test]
async fn first_party_enable_preserves_xep0357_publish_options_in_registration_and_jobs() {
    let store = store().await;
    let owner = owner();
    let node = store.ensure_node(&owner, "web").await.expect("push node");
    store
        .upsert_device(
            &owner,
            PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test"),
        )
        .await
        .expect("device");
    let publish_options = publish_options_with_field("secret", "server-secret");
    let registration_store = DatabasePushRegistrationStore::new(store.database())
        .await
        .expect("registration store");
    store
        .register_first_party_node_for_owner(
            &owner,
            "push.example.com",
            node.node(),
            Some(&publish_options),
        )
        .await
        .expect("first-party registration");
    let registrations = registration_store
        .get_for_user(&owner.to_string())
        .await
        .expect("registrations");

    store
        .publish_notification_from_user_server_with_publish_options(
            node.node(),
            &notification_item("publish-options-job"),
            &owner,
            registrations[0].publish_options.as_ref(),
        )
        .await
        .expect("publish with options");
    let mut rows = query(
        &store,
        "SELECT publish_options_xml FROM push_publish_jobs WHERE node = ? AND item_id = ?",
        db_params![node.node(), "publish-options-job"],
    )
    .await;
    let row = rows
        .next()
        .await
        .expect("job options row")
        .expect("job options row");
    let job_options_xml: Option<String> = row.get(0).expect("job options xml");

    assert_eq!(registrations.len(), 1);
    assert!(registrations[0].publish_options.is_some());
    assert!(job_options_xml
        .expect("job publish options")
        .contains("server-secret"));
}

#[tokio::test]
async fn xep0357_pubsub_iq_publish_requires_live_first_party_registration() {
    let store = store().await;
    let owner = owner();
    let node = store.ensure_node(&owner, "web").await.expect("push node");
    store
        .upsert_device(
            &owner,
            PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test"),
        )
        .await
        .expect("device");
    DatabasePushRegistrationStore::new(store.database())
        .await
        .expect("registration store");
    let publish_options = publish_options_with_field("secret", "server-secret");
    store
        .register_first_party_node_for_owner(
            &owner,
            "push.example.com",
            node.node(),
            Some(&publish_options),
        )
        .await
        .expect("first-party registration");
    let iq = xep0357_pubsub_publish_iq(
        "push.example.com",
        &owner,
        node.node(),
        &notification_item("xep-pubsub-iq"),
        Some(&publish_options),
    );

    let result = store
        .publish_xep0357_pubsub_iq_from_user_server("push.example.com", &iq, &owner)
        .await
        .expect("server-origin PubSub publish");
    let attempts = store
        .delivery_attempts_for_node(node.node())
        .await
        .expect("attempts");
    let mut rows = query(
        &store,
        "SELECT push_service_jid, publish_options_xml FROM push_publish_jobs \
         WHERE node = ? AND item_id = ?",
        db_params![node.node(), "xep-pubsub-iq"],
    )
    .await;
    let row = rows.next().await.expect("job row").expect("job row");
    let stored_service: Option<String> = row.get(0).expect("service jid");
    let stored_options: Option<String> = row.get(1).expect("publish options");

    assert_eq!(result.attempted_devices(), 1);
    assert_eq!(attempts.len(), 1);
    assert_eq!(stored_service.as_deref(), Some("push.example.com"));
    assert!(stored_options
        .expect("stored publish options")
        .contains("server-secret"));

    store
        .remove_registered_nodes_for_owner(&owner, "push.example.com", Some(node.node()))
        .await
        .expect("disable registration");
    let stale_iq = xep0357_pubsub_publish_iq(
        "push.example.com",
        &owner,
        node.node(),
        &notification_item("after-registration-disable"),
        Some(&publish_options),
    );
    let error = store
        .publish_xep0357_pubsub_iq_from_user_server("push.example.com", &stale_iq, &owner)
        .await
        .expect_err("disabled registration rejects stale publish snapshot");

    assert_item_not_found(error);
    assert!(store
        .delivery_attempts_for_node(node.node())
        .await
        .expect("attempts after disable")
        .iter()
        .all(|attempt| attempt.item_id() != "after-registration-disable"));
}

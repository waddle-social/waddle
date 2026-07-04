//! Shared helpers for the Push Service inline test modules.

use jid::BareJid;
use minidom::Element;
use waddle_xmpp::pubsub::PubSubItem;
use waddle_xmpp::xep::xep0357::NS_PUSH;
use waddle_xmpp::XmppError;

use crate::db::{Database, IntoParams};

use super::store::DatabasePushServiceStore;

pub(super) async fn store() -> DatabasePushServiceStore {
    DatabasePushServiceStore::new(
        Database::in_memory("push-service")
            .await
            .expect("push service db"),
    )
    .await
    .expect("push service store")
}

pub(super) fn owner() -> BareJid {
    "alice@example.com".parse().expect("owner jid")
}

pub(super) fn notification_item(item_id: &str) -> PubSubItem {
    PubSubItem::new(
        Some(item_id.to_string()),
        Some(Element::builder("notification", NS_PUSH).build()),
    )
}

pub(super) fn publish_options_with_field(var: &str, value: &str) -> Element {
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

pub(super) async fn scalar_i64(
    store: &DatabasePushServiceStore,
    sql: &str,
    params: impl IntoParams,
) -> i64 {
    let mut rows = store.query(sql, params).await.expect("scalar query");
    let row = rows.next().await.expect("scalar row").expect("scalar row");
    row.get(0).expect("scalar value")
}

pub(super) fn assert_item_not_found(error: XmppError) {
    assert!(matches!(
        error,
        XmppError::Stanza {
            condition: waddle_xmpp::StanzaErrorCondition::ItemNotFound,
            ..
        }
    ));
}

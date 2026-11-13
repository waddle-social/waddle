//! Delivery to the exact binding the account coordinator names: the
//! connection registry is only the transport index, and an entry that
//! carries any other binding of the full JID receives nothing.

use super::{
    chat_msg, jid, offline_pass_deps, pipelined_dispatcher, Arc, BlockingStorage,
    ConnectionRegistry, InboxStorage, MamStorage, Stanza,
};
use crate::server::routes::interpret::{deliver_to_local_binding, FullJidDeliveryOutcome};
use waddle_xmpp::ownership::{BindingRef, ConnectionRef, NodeLeaseIdentity};
use waddle_xmpp::registry::{ConnectionEntry, DeliveryKind};

struct Fixture {
    mam: Arc<dyn MamStorage>,
    inbox: Arc<dyn InboxStorage>,
    blocking: Arc<dyn BlockingStorage>,
    dispatcher: Arc<super::StanzaDispatcher>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            mam: Arc::new(waddle_xmpp::mam::storage::InMemoryMamStorage::new()),
            inbox: Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new()),
            blocking: Arc::new(waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new()),
            dispatcher: pipelined_dispatcher(),
        }
    }
}

fn message_to(target: &jid::FullJid) -> Stanza {
    Stanza::Message(chat_msg(
        jid("romeo@example.com/orchard"),
        jid::Jid::from(target.clone()),
        "bound",
    ))
}

#[tokio::test]
async fn a_stale_binding_is_refused_and_the_current_binding_receives_the_stanza() {
    let fixture = Fixture::new();
    let registry = ConnectionRegistry::new();
    let target: jid::FullJid = "juliet@example.com/balcony".parse().expect("jid");
    let connection = ConnectionRef::fresh(NodeLeaseIdentity::local_process());
    let stale = BindingRef::fresh(connection.clone());
    let current = BindingRef::fresh(connection);
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    registry.register_entry(
        target.clone(),
        ConnectionEntry::new_for_binding(tx, current.clone()),
    );
    let deps = offline_pass_deps(
        &registry,
        &fixture.mam,
        &fixture.inbox,
        &fixture.blocking,
        &fixture.dispatcher,
    );

    // A stanza selected for the predecessor binding is refused: the entry
    // that holds the full JID now carries another binding.
    for live_only in [false, true] {
        assert_eq!(
            deliver_to_local_binding(
                &deps,
                &target,
                &stale,
                &message_to(&target),
                DeliveryKind::PeerStanza,
                None,
                live_only,
            )
            .await,
            FullJidDeliveryOutcome::Unavailable
        );
    }
    assert!(rx.try_recv().is_err(), "the replacement received nothing");

    assert_eq!(
        deliver_to_local_binding(
            &deps,
            &target,
            &current,
            &message_to(&target),
            DeliveryKind::PeerStanza,
            None,
            false,
        )
        .await,
        FullJidDeliveryOutcome::Delivered
    );
    let outbound = rx
        .try_recv()
        .expect("the current binding receives the stanza");
    assert_eq!(outbound.kind, DeliveryKind::PeerStanza);
    assert!(rx.try_recv().is_err(), "exactly one copy");
}

#[tokio::test]
async fn a_binding_without_a_connection_is_not_delivered_live() {
    let fixture = Fixture::new();
    let registry = ConnectionRegistry::new();
    let target: jid::FullJid = "juliet@example.com/balcony".parse().expect("jid");
    let binding = BindingRef::fresh(ConnectionRef::fresh(NodeLeaseIdentity::local_process()));
    let deps = offline_pass_deps(
        &registry,
        &fixture.mam,
        &fixture.inbox,
        &fixture.blocking,
        &fixture.dispatcher,
    );
    // No connection and no stream-management registry: nothing takes it.
    assert_eq!(
        deliver_to_local_binding(
            &deps,
            &target,
            &binding,
            &message_to(&target),
            DeliveryKind::DirectFrame,
            None,
            false,
        )
        .await,
        FullJidDeliveryOutcome::Unavailable
    );
}

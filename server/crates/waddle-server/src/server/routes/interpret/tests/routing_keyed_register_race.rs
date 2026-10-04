use super::*;

#[tokio::test]
async fn keyed_fallback_never_enters_an_actor_registered_after_socket_lookup() {
    use super::super::routing::{deliver_direct_to_full, deliver_peer_to_full};
    use waddle_xmpp::registry::UserRegistryActor;

    for direct in [false, true] {
        let connections = test_registry();
        let users = UserRegistryActor::spawn(UserRegistryActor::new());
        let target: jid::FullJid = "bob@example.com/phone".parse().expect("target");
        // The owning-socket attempt missed the resource before the bind finished.
        assert!(connections.get_entry(&target).is_none());
        let (sender, mut receiver) = tokio::sync::mpsc::channel(2);
        register_into_both_tiers(&connections, &users, &target, sender).await;

        let stanza = Stanza::Message(chat_msg(
            jid("alice@example.com/web"),
            target.clone().into(),
            "registered during routing",
        ));
        let context = SmIngressAppendContext {
            message_key: waddle_xmpp::ingress::MessageKey::new(),
            receipt: crate::ingress::EffectReceiptKey {
                kind: crate::ingress_substrate::EffectReceiptKind::from_storage(
                    waddle_xmpp::ingress::IngressEffectKind::RouteDirect.storage_tag(),
                ),
                semantic_identity_hash: [1; 32],
            },
            received_at: None,
            archive_positions: Vec::new(),
            dispatch_stream: None,
        };
        let outcome = if direct {
            deliver_direct_to_full(Some(&users), None, &target, &stanza, Some(&context)).await
        } else {
            deliver_peer_to_full(Some(&users), None, &target, &stanza, Some(&context)).await
        };
        assert_eq!(outcome, FullJidDeliveryOutcome::Unavailable);
        assert!(
            receiver.try_recv().is_err(),
            "keyed fallback must not send through the new actor entry"
        );

        // Establish that the same actor is live: an unkeyed attempt can reach it.
        let outcome = if direct {
            deliver_direct_to_full(Some(&users), None, &target, &stanza, None).await
        } else {
            deliver_peer_to_full(Some(&users), None, &target, &stanza, None).await
        };
        assert_eq!(outcome, FullJidDeliveryOutcome::Delivered);
        assert!(
            receiver.try_recv().is_ok(),
            "actor registration must actually be usable"
        );
        assert!(
            receiver.try_recv().is_err(),
            "only the unkeyed control was sent"
        );
    }
}

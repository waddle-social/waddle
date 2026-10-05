//! The registered socket endpoint retains ingress authority without archive ordering.
#[path = "live_invitation.rs"]
mod invitation;

use super::*;
use crate::ingress::{
    commit::commit_submission, identity::IngressAppendObligationRef, test_support::IngressFixture,
};
use crate::server::routes::websocket::{
    tests::create_test_websocket_state_with_db_pool_and_ingress, WebSocketState,
};
use waddle_xmpp::ingress::{EffectMessageIdentity, IngressEffectIntent};

struct SocketFixture {
    state: Arc<WebSocketState>,
    bridge: Arc<OrderedRelayDeliveryBridge>,
    services: Arc<OrderedRelayDeliveryServices>,
    frame: RelayDeliverRemoteResourceFrame,
    receiver: mpsc::Receiver<OutboundStanza>,
}

impl SocketFixture {
    async fn new(fixture: &IngressFixture) -> Self {
        let pool = crate::db::DatabasePool::new(
            crate::db::DatabaseConfig::new(fixture.db.driver(), fixture.db.database_url()),
            crate::db::PoolConfig,
        )
        .await
        .expect("shared database");
        let state = create_test_websocket_state_with_db_pool_and_ingress(
            Arc::new(pool),
            Arc::new(fixture.authority().await),
        )
        .await;
        let mut services = services_with_claims(
            origin_identity(),
            receiver_identity(),
            receiver_identity(),
            test_peer_id(),
        )
        .await;
        services.web_socket_state = Arc::downgrade(&state);
        let services = Arc::new(services);
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        bridge.wire(Arc::clone(&services));
        let target: jid::FullJid = "juliet@example.com/phone".parse().expect("recipient");
        let (sender, receiver) = mpsc::channel(4);
        let entry = ConnectionEntry::new(sender);
        let owner = entry.carbons_handle();
        services
            .connection_registry
            .register_entry(target.clone(), entry);
        let registration_id = bridge
            .test_insert_remote_socket_registration(
                target.clone(),
                owner,
                NodeId::new("remote-user-owner".to_owned()),
            )
            .await;
        let intent = IngressEffectIntent::RouteDirect {
            prepared: None,
            recipient: target.to_bare(),
            fanout: vec![target.clone()],
            route_identity: EffectMessageIdentity::capture_ordinal(1),
        };
        let receipt = crate::ingress::receipt_key(&intent).expect("receipt");
        let mut submission = fixture.submission(None, "one registered socket delivery");
        submission.plan.intents = vec![intent];
        let decision = commit_submission(&fixture.uow, &submission, 1)
            .await
            .expect("canonical message");
        let frame = RelayDeliverRemoteResourceFrame {
            frame: RemoteResourceOutboundFrame {
                jid: target,
                registration_id,
                stanza: RemoteStanza(Stanza::Message(submission.plan.sanitized_message)),
                kind: DeliveryKind::PeerStanza,
                ingress_append: Some(IngressAppendObligationRef {
                    archive_positions: Vec::new(),
                    dispatch_stream: None,
                    message_key: decision.message_key.expect("canonical key"),
                    sender_bare: submission.sender.to_bare(),
                    receipt,
                    received_at: None,
                }),
            },
            trace: RelayTraceContext::default(),
        };
        Self {
            state,
            bridge,
            services,
            frame,
            receiver,
        }
    }

    async fn deliver(&self) -> RelayRemoteResourceFrameStatus {
        self.bridge
            .deliver_remote_resource_frame_on_socket(self.frame.clone())
            .await
            .status
    }

    async fn close(self) {
        self.state
            .deps
            .protocol
            .ingress
            .drain_and_join(Duration::from_secs(1))
            .await;
    }
}

async fn retry_without_receipt_emits_once(fixture: IngressFixture) {
    let mut socket = SocketFixture::new(&fixture).await;
    assert_eq!(
        socket.deliver().await,
        RelayRemoteResourceFrameStatus::Delivered
    );
    let outbound = socket
        .receiver
        .try_recv()
        .expect("one accepted socket frame");
    assert!(
        outbound.ingress_append.is_some(),
        "detach drain retains authority"
    );
    assert_eq!(fixture.count("ingress_send_attempts").await, 1);
    assert_eq!(fixture.count("ingress_delivery_receipts").await, 0);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    assert_eq!(
        socket.deliver().await,
        RelayRemoteResourceFrameStatus::Delivered
    );
    assert!(
        socket.receiver.try_recv().is_err(),
        "lost receipt cannot resend"
    );

    // A successful send followed by socket retirement still repairs the reply;
    // Unavailable here would incorrectly permit detached fallback.
    socket
        .bridge
        .remote_socket_resources
        .lock()
        .await
        .remove(&socket.frame.frame.jid);
    assert_eq!(
        socket.deliver().await,
        RelayRemoteResourceFrameStatus::Delivered
    );
    assert!(socket.receiver.try_recv().is_err());
    socket.close().await;
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_remote_socket_retry_without_receipt_emits_once() {
    retry_without_receipt_emits_once(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_remote_socket_retry_without_receipt_emits_once() {
    if let Some(fixture) = IngressFixture::postgres("socket_send_retry").await {
        retry_without_receipt_emits_once(fixture).await;
    }
}

async fn forged_obligation_cannot_send(fixture: IngressFixture) {
    let mut socket = SocketFixture::new(&fixture).await;
    let authorized = socket.frame.clone();
    socket
        .frame
        .frame
        .ingress_append
        .as_mut()
        .expect("obligation")
        .receipt
        .semantic_identity_hash = [42; 32];
    assert_eq!(
        socket.deliver().await,
        RelayRemoteResourceFrameStatus::Backpressure
    );
    assert!(socket.receiver.try_recv().is_err());

    socket.frame = authorized.clone();
    let other: jid::FullJid = "juliet@example.com/not-frozen"
        .parse()
        .expect("other resource");
    let (sender, mut receiver) = mpsc::channel(1);
    let entry = ConnectionEntry::new(sender);
    let owner = entry.carbons_handle();
    socket
        .services
        .connection_registry
        .register_entry(other.clone(), entry);
    let registration_id = socket
        .bridge
        .test_insert_remote_socket_registration(
            other.clone(),
            owner,
            NodeId::new("remote-user-owner".to_owned()),
        )
        .await;
    socket.frame.frame.jid = other;
    socket.frame.frame.registration_id = registration_id;
    assert_eq!(
        socket.deliver().await,
        RelayRemoteResourceFrameStatus::Backpressure
    );
    assert!(
        receiver.try_recv().is_err(),
        "unrecorded resource cannot receive"
    );

    socket.frame = authorized.clone();
    let Stanza::Message(message) = &mut socket.frame.frame.stanza.0 else {
        panic!("message frame");
    };
    message
        .bodies
        .insert(Lang::new(), "substituted payload".into());
    assert_eq!(
        socket.deliver().await,
        RelayRemoteResourceFrameStatus::Backpressure
    );
    assert!(
        socket.receiver.try_recv().is_err(),
        "canonical body cannot be replaced"
    );
    assert_eq!(fixture.count("ingress_send_attempts").await, 0);

    socket.frame = authorized;
    assert_eq!(
        socket.deliver().await,
        RelayRemoteResourceFrameStatus::Delivered
    );
    socket
        .receiver
        .try_recv()
        .expect("valid obligation still delivers");
    socket.close().await;
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_remote_socket_forged_obligation_without_archive_cannot_send() {
    forged_obligation_cannot_send(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_remote_socket_forged_obligation_without_archive_cannot_send() {
    if let Some(fixture) = IngressFixture::postgres("socket_send_authority").await {
        forged_obligation_cannot_send(fixture).await;
    }
}

#[tokio::test]
async fn remote_socket_ambiguous_send_blocks_only_until_its_recovery_deadline() {
    let fixture = IngressFixture::sqlite().await;
    let mut socket = SocketFixture::new(&fixture).await;
    fixture.execute("CREATE TRIGGER fail_socket_send_completion BEFORE UPDATE ON ingress_send_attempts WHEN NEW.state = 2 BEGIN SELECT RAISE(ABORT, 'lost completion'); END", ()).await;
    assert_eq!(
        socket.deliver().await,
        RelayRemoteResourceFrameStatus::Backpressure
    );
    socket
        .receiver
        .try_recv()
        .expect("send occurred before completion failure");
    assert_eq!(
        socket.deliver().await,
        RelayRemoteResourceFrameStatus::Backpressure
    );
    assert!(
        socket.receiver.try_recv().is_err(),
        "unexpired start excludes retry"
    );
    fixture
        .execute("UPDATE ingress_send_attempts SET expires_at_ms = 0", ())
        .await;
    assert_eq!(
        socket.deliver().await,
        RelayRemoteResourceFrameStatus::Backpressure
    );
    socket
        .receiver
        .try_recv()
        .expect("expired uncertainty permits a bounded retry");
    socket
        .bridge
        .remote_socket_resources
        .lock()
        .await
        .remove(&socket.frame.frame.jid);
    assert_eq!(
        socket.deliver().await,
        RelayRemoteResourceFrameStatus::Backpressure
    );
    assert_eq!(fixture.count("ingress_send_attempts").await, 1);
    assert_eq!(fixture.count("ingress_delivery_receipts").await, 0);
    assert_eq!(fixture.count("sm_ingress_appends").await, 0);
    socket.close().await;
    fixture.close().await;
}

#[tokio::test]
async fn remote_socket_owner_replaced_during_send_lease_cannot_receive_old_frame() {
    let fixture = IngressFixture::sqlite().await;
    let mut socket = SocketFixture::new(&fixture).await;
    let obligation = socket
        .frame
        .frame
        .ingress_append
        .as_ref()
        .expect("obligation");
    let gate = crate::ingress::live_delivery::test_hooks::pause_after_start(
        obligation.message_key,
        socket.frame.frame.jid.clone(),
    );
    let (sender, mut replacement_receiver) = mpsc::channel(1);
    let replacement = ConnectionEntry::new(sender);
    let replace_owner = async {
        tokio::time::timeout(Duration::from_secs(1), gate.wait_until_reached())
            .await
            .expect("delivery reached durable start");
        socket
            .services
            .connection_registry
            .register_entry(socket.frame.frame.jid.clone(), replacement);
        gate.release();
    };
    let (status, ()) = tokio::join!(socket.deliver(), replace_owner);
    assert_eq!(status, RelayRemoteResourceFrameStatus::Unavailable);
    assert!(
        socket.receiver.try_recv().is_err(),
        "retired owner cannot receive"
    );
    assert!(
        replacement_receiver.try_recv().is_err(),
        "captured owner cannot redirect to replacement"
    );
    assert_eq!(
        fixture.count("ingress_send_attempts").await,
        0,
        "proven not enqueued releases the attempt"
    );
    socket.close().await;
    fixture.close().await;
}

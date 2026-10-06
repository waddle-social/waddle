//! Exercise the production detach drain after the relay actor queues a frame.
use super::super::replay::{drain_outbound_into_replay, PendingRowDrainPolicy, ReplayDrainSink};
use super::*;

pub(crate) async fn drain_registered_remote_frame(
    sm: Arc<InMemorySmSessionRegistry>,
    target: &FullJid,
    receiver: &mut mpsc::Receiver<OutboundStanza>,
) {
    let state = create_test_websocket_state_with_clustering(
        crate::clustering::ClusteringHandles::default(),
        sm,
    )
    .await;
    let stream = target.to_string();
    let mut sm_state = StreamManagementState::default();
    sm_state.enable(stream.clone(), true, Some(300));
    let mut drained_appends = Vec::new();
    receiver.close();
    drain_outbound_into_replay(
        &state,
        None,
        &mut sm_state,
        None,
        receiver,
        ReplayDrainSink {
            detached_stream_id: Some(&stream),
            pending_row_policy: PendingRowDrainPolicy::PreserveForReplay,
            drained_appends: &mut drained_appends,
        },
    )
    .await;
    assert!(
        drained_appends.is_empty(),
        "timestamp-only frame cannot acquire keyed ingress custody"
    );
    assert_eq!(sm_state.queue_len(), 1);
}

//! XEP-0198 §4: even a single pending delivery prompts its durable custody ACK.

pub mod pending_delivery_ws_support;

use pending_delivery_ws_support::{ack, enable, message, presence, recv, PendingFixture};
use waddle_xmpp::stream_management::{DEFAULT_ACK_REQUEST_THRESHOLD, SM_NS};

#[tokio::test]
async fn single_pending_stanza_requests_ack_immediately_and_deletes_only_after_ack() {
    let fixture = PendingFixture::start().await;
    let mut sender = fixture.connect("alice", "sender").await;
    fixture.offer(&mut sender, "single-pending").await;
    let queued = fixture.wait_rows(|rows| rows.len() == 1).await;
    assert!(queued[0].payload.is_archived());
    assert!(queued[0].flushed_in_session.is_none());

    let mut recipient = fixture.connect("bob", "phone").await;
    let session = enable(&mut recipient).await;
    presence(&mut recipient, 0).await;
    let mut handled = 0;
    message(&mut recipient, &mut handled, "single-pending").await;
    assert!(handled < DEFAULT_ACK_REQUEST_THRESHOLD);

    // Read the very next frame: accepting a later <r/> after some unrelated
    // stanza would not prove the pending-delivery write requests its own ACK.
    let request = recv(&mut recipient).await;
    assert!(request.is("r", SM_NS), "immediate ACK request: {request:?}");
    let retained = fixture.rows().await;
    assert_eq!(
        retained.len(),
        1,
        "wire delivery alone does not delete custody"
    );
    assert_eq!(retained[0].id, queued[0].id);
    assert_eq!(retained[0].flushed_in_session.as_ref(), Some(&session));
    assert_eq!(retained[0].outbound_sequence, Some(handled));

    ack(&mut recipient, handled).await;
    fixture
        .wait_rows(<[waddle_xmpp::pending_delivery::PendingRow]>::is_empty)
        .await;
}

//! T1-F18-04: Streaming inference (>15s) decoupled from `STANZA_HANDLER_WEDGE_TIMEOUT`.
//! Proves progressive chunk delivery across a >15s stream using tokio paused time
//! and clean cancellation on `CancellationToken` drain without triggering the 15s
//! synchronous stanza wedge timeout.

use std::time::Duration;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use waddle_server::server::spawn_decoupled_streaming_inference;

#[tokio::test(start_paused = true)]
async fn streaming_inference_exceeds_15s_wedge_timeout_without_aborting_and_cancels_on_drain() {
    let shutdown = CancellationToken::new();
    let (mut session, handle) = spawn_decoupled_streaming_inference(
        "stream-wedge-1",
        8,
        shutdown.clone(),
        |tx: mpsc::Sender<Vec<u8>>, cancel_rx| async move {
            // Emit first chunk immediately.
            if tx.send(b"chunk-0".to_vec()).await.is_err() {
                return;
            }
            // Simulate a 20-second provider stream (> STANZA_HANDLER_WEDGE_TIMEOUT = 15s).
            tokio::time::sleep(Duration::from_secs(20)).await;
            if *cancel_rx.borrow() {
                return;
            }
            let _ = tx.send(b"chunk-after-20s".to_vec()).await;
        },
    );

    let first = session
        .chunk_rx
        .recv()
        .await
        .expect("first chunk delivered immediately");
    assert_eq!(first, b"chunk-0");

    // Advance virtual time well past STANZA_HANDLER_WEDGE_TIMEOUT (15s).
    tokio::time::advance(Duration::from_secs(21)).await;
    let second = session
        .chunk_rx
        .recv()
        .await
        .expect("chunk after 20s must still be delivered without 15s wedge timeout");
    assert_eq!(second, b"chunk-after-20s");
    handle.await.expect("stream task joins cleanly");

    // Now prove drain shutdown cancels an active slow stream immediately.
    let shutdown2 = CancellationToken::new();
    let (mut session2, handle2) = spawn_decoupled_streaming_inference(
        "stream-wedge-drain",
        8,
        shutdown2.clone(),
        |tx: mpsc::Sender<Vec<u8>>, mut cancel_rx| async move {
            let _ = tx.send(b"initial".to_vec()).await;
            let _ = cancel_rx.changed().await;
        },
    );
    assert_eq!(
        session2.chunk_rx.recv().await.as_deref(),
        Some(b"initial".as_slice())
    );
    shutdown2.cancel();
    assert!(
        session2.chunk_rx.recv().await.is_none(),
        "drain cancellation closes stream promptly"
    );
    handle2.await.expect("canceled task joins cleanly");
}

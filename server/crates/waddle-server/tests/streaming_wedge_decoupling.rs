//! T1-F18-04: Streaming inference (>15s) decoupled from `STANZA_HANDLER_WEDGE_TIMEOUT`.
//! Proves progressive chunk delivery across a >15s stream using tokio paused time
//! and clean cancellation on `CancellationToken` drain without triggering the 15s
//! synchronous stanza wedge timeout.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tempfile::TempDir;
use tokio::sync::{mpsc, Notify};
use tokio_util::sync::CancellationToken;
use waddle_extensions::config::{ExtensionConfig, ExtensionModuleConfig, RuntimeLimits};
use waddle_extensions::host_tools::{DenyingExtensionHostTools, InvocationContext};
use waddle_extensions::runtime::{
    ExtensionInferenceHost, HostInferenceChunk, HostInferenceError, HostInferenceRequest,
    OpenedInferenceStream, StreamCancelHandle, StreamItem, VerifiedInferenceUsage,
};
use waddle_extensions::{ExtensionCapability, ExtensionManager};
use waddle_server::server::{
    run_extension_command_decoupled_for_test, spawn_decoupled_streaming_inference,
};

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

#[tokio::test(flavor = "current_thread")]
async fn wired_extension_command_streams_past_15s_wedge_timeout_and_cancels_on_drain() {
    struct SlowStreamingInferenceHost {
        first_chunk_emitted: Arc<Notify>,
        cancel_rx_slot: Mutex<Option<tokio::sync::watch::Receiver<bool>>>,
        delay: Duration,
    }

    #[async_trait]
    impl ExtensionInferenceHost for SlowStreamingInferenceHost {
        async fn start_stream(
            &self,
            _context: &InvocationContext,
            _request: HostInferenceRequest,
        ) -> Result<OpenedInferenceStream, HostInferenceError> {
            let (tx, rx) = mpsc::channel(8);
            let (cancel, cancel_rx) = StreamCancelHandle::new();
            *self.cancel_rx_slot.lock().expect("lock") = Some(cancel_rx);
            let emitted = Arc::clone(&self.first_chunk_emitted);
            let delay = self.delay;
            tokio::spawn(async move {
                let _ = tx
                    .send(StreamItem::Chunk(HostInferenceChunk {
                        delta: b"first-".to_vec(),
                        sequence: 0,
                        finish_reason: None,
                    }))
                    .await;
                emitted.notify_one();
                tokio::time::sleep(delay).await;
                let _ = tx
                    .send(StreamItem::Chunk(HostInferenceChunk {
                        delta: b"after-20s".to_vec(),
                        sequence: 1,
                        finish_reason: Some("stop".to_owned()),
                    }))
                    .await;
                let _ = tx
                    .send(StreamItem::EndOfStream(VerifiedInferenceUsage {
                        provider: "openai".to_owned(),
                        model: "gpt-4o-mini".to_owned(),
                        input_tokens: 10,
                        output_tokens: 2,
                        cost_microcents: 100,
                    }))
                    .await;
            });
            Ok(OpenedInferenceStream {
                rx,
                cancel,
                max_chunk_bytes: 1024,
            })
        }
    }

    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../waddle-extensions/tests/fixtures/inference_stream.wat");
    let cache_dir = TempDir::new().expect("tempdir");

    let config = ExtensionConfig {
        enabled: true,
        cache_dir: cache_dir.path().display().to_string(),
        modules: vec![ExtensionModuleConfig {
            name: "ai-chatbot".to_owned(),
            room_observation: None,
            runtime_limits: RuntimeLimits {
                invocation_timeout_ms: 60_000,
                ..RuntimeLimits::default()
            },
            registry: String::new(),
            digest: None,
            tag: None,
            namespace: "urn:waddle:ai-chatbot:1".to_owned(),
            config: serde_json::json!({}),
            capability_grants: vec![ExtensionCapability::Commands],
            allowed_http_origins: Vec::new(),
            provider_room_grants: Vec::new(),
            config_secret_files: Default::default(),
            local_path: Some(fixture_path.display().to_string()),
        }],
    };

    // 1. Live `handle_xmpp_frame_with_admission` -> `handle_command_iq` ->
    //    `register_extension_commands` streams progressive chunks across >15s, delivers
    //    the final XEP-0050 IQ result via UserRegistryActor / deliver_direct_to_full,
    //    and drains `inference_stream_tasks` cleanly.
    let first_chunk_emitted = Arc::new(Notify::new());
    let host = Arc::new(SlowStreamingInferenceHost {
        first_chunk_emitted: Arc::clone(&first_chunk_emitted),
        cancel_rx_slot: Mutex::new(None),
        delay: Duration::from_secs(20),
    });
    let manager = Arc::new(
        ExtensionManager::from_config_with_hosts(
            config.clone(),
            Arc::new(DenyingExtensionHostTools),
            host,
        )
        .await
        .expect("extension manager"),
    );

    let requester: jid::FullJid = "alice@example.com/web".parse().expect("full jid");
    let mut handle = run_extension_command_decoupled_for_test(
        manager,
        "urn:waddle:extension:1:ai-chatbot".to_owned(),
        requester,
        5,
    )
    .await;
    assert_eq!(
        handle.immediate_frames,
        Vec::<String>::new(),
        "synchronous frame dispatch must return immediately with no 15s wedge timeout reply"
    );
    let mut session = handle
        .session
        .take()
        .expect("active StreamingInferenceSession recorded on WsConnState");

    // Progressive chunk must arrive BEFORE advancing past the 15s wedge timeout.
    first_chunk_emitted.notified().await;
    let first_progressive = tokio::time::timeout(Duration::from_secs(2), session.chunk_rx.recv())
        .await
        .expect("first progressive chunk delivered promptly")
        .expect("first chunk present");
    assert_eq!(first_progressive, b"first-");

    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(21)).await;

    let second_progressive = tokio::time::timeout(Duration::from_secs(2), session.chunk_rx.recv())
        .await
        .expect("second progressive chunk delivered after 20s")
        .expect("second chunk present");
    assert_eq!(second_progressive, b"after-20s");

    let outbound = tokio::time::timeout(Duration::from_secs(2), handle.outbound_rx.recv())
        .await
        .expect("IQ reply delivered after 20s")
        .expect("outbound stanza present");
    tokio::time::resume();
    let xml = format!("{:?}", outbound);
    assert!(
        xml.contains("first-after-20s"),
        "expected assembled 20s stream output in delivered XEP-0050 reply: {xml}"
    );
    handle.inference_stream_tasks.close();
    tokio::time::timeout(Duration::from_secs(2), handle.inference_stream_tasks.wait())
        .await
        .expect("inference_stream_tasks drained cleanly");

    // 2a. Client disconnect (`drop(session)`) cancels the live command stream synchronously.
    let first_chunk_disconnect = Arc::new(Notify::new());
    let host_disconnect = Arc::new(SlowStreamingInferenceHost {
        first_chunk_emitted: Arc::clone(&first_chunk_disconnect),
        cancel_rx_slot: Mutex::new(None),
        delay: Duration::from_secs(60),
    });
    let manager_disconnect = Arc::new(
        ExtensionManager::from_config_with_hosts(
            config.clone(),
            Arc::new(DenyingExtensionHostTools),
            Arc::clone(&host_disconnect) as Arc<dyn ExtensionInferenceHost>,
        )
        .await
        .expect("extension manager"),
    );
    let requester_disconnect: jid::FullJid = "charlie@example.com/web".parse().expect("full jid");
    let mut handle_disconnect = run_extension_command_decoupled_for_test(
        manager_disconnect,
        "urn:waddle:extension:1:ai-chatbot".to_owned(),
        requester_disconnect,
        5,
    )
    .await;
    assert_eq!(handle_disconnect.immediate_frames, Vec::<String>::new());
    let mut session_disconnect = handle_disconnect
        .session
        .take()
        .expect("disconnect StreamingInferenceSession present");
    first_chunk_disconnect.notified().await;
    let disconnect_chunk =
        tokio::time::timeout(Duration::from_secs(2), session_disconnect.chunk_rx.recv())
            .await
            .expect("first chunk delivered")
            .expect("chunk bytes present");
    assert_eq!(disconnect_chunk, b"first-");
    let cancel_rx_disconnect = host_disconnect
        .cancel_rx_slot
        .lock()
        .expect("lock")
        .take()
        .expect("start_stream invoked");
    assert!(!*cancel_rx_disconnect.borrow());
    drop(session_disconnect);
    let _ =
        tokio::time::timeout(Duration::from_secs(2), handle_disconnect.outbound_rx.recv()).await;
    handle_disconnect.inference_stream_tasks.close();
    tokio::time::timeout(
        Duration::from_secs(2),
        handle_disconnect.inference_stream_tasks.wait(),
    )
    .await
    .expect("disconnect task drained");
    assert!(
        *cancel_rx_disconnect.borrow(),
        "dropping StreamingInferenceSession must propagate cancel_rx and cancel InferenceStream"
    );

    // 2b. GracefulShutdown::trigger_stop() (WADDLE_DRAIN_TIMEOUT_SECS) immediately aborts the
    //     active stream, drops Store<HostState> (InferenceStream::Drop), and drains the tracker.
    let first_chunk_drain = Arc::new(Notify::new());
    let host_drain = Arc::new(SlowStreamingInferenceHost {
        first_chunk_emitted: Arc::clone(&first_chunk_drain),
        cancel_rx_slot: Mutex::new(None),
        delay: Duration::from_secs(60),
    });
    let manager_drain = Arc::new(
        ExtensionManager::from_config_with_hosts(
            config,
            Arc::new(DenyingExtensionHostTools),
            Arc::clone(&host_drain) as Arc<dyn ExtensionInferenceHost>,
        )
        .await
        .expect("extension manager"),
    );
    let requester2: jid::FullJid = "bob@example.com/web".parse().expect("full jid");
    let mut handle_drain = run_extension_command_decoupled_for_test(
        manager_drain,
        "urn:waddle:extension:1:ai-chatbot".to_owned(),
        requester2,
        5,
    )
    .await;
    assert_eq!(handle_drain.immediate_frames, Vec::<String>::new());
    let mut session_drain = handle_drain
        .session
        .take()
        .expect("drain StreamingInferenceSession present");
    first_chunk_drain.notified().await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), session_drain.chunk_rx.recv())
            .await
            .expect("first chunk delivered")
            .as_deref(),
        Some(b"first-".as_slice())
    );

    let cancel_rx = host_drain
        .cancel_rx_slot
        .lock()
        .expect("lock")
        .take()
        .expect("start_stream invoked");
    assert!(!*cancel_rx.borrow());

    handle_drain.shutdown.trigger_stop();
    let _ = tokio::time::timeout(Duration::from_secs(2), handle_drain.outbound_rx.recv()).await;
    handle_drain.inference_stream_tasks.close();
    tokio::time::timeout(
        Duration::from_secs(2),
        handle_drain.inference_stream_tasks.wait(),
    )
    .await
    .expect("graceful shutdown inference_stream_tasks drained");
    assert!(
        *cancel_rx.borrow(),
        "drain cancellation must drop Store<HostState> and synchronously cancel InferenceStream"
    );
}

//! Feature 18 tests for `InferenceStream`, synchronous `Drop` cancellation,
//! chunk boundaries, narrowed WASI linker boundaries, guest stderr/log
//! suppression, and removal of `outbound.http.request` from inference guests.

use std::collections::HashSet;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc;
use waddle_extensions::host_tools::{
    DenyingExtensionHostTools, ExtensionHostTools, InvocationContext, InvocationKind,
};
use waddle_extensions::runtime::inference::{
    HostInferenceChunk, HostInferenceError, HostInferenceErrorCode, HostInferenceMessage,
    HostInferenceRequest, InferenceCredentialRef, InferenceStream, OpenedInferenceStream,
    StreamCancelHandle, StreamItem, VerifiedInferenceUsage, DEFAULT_MAX_STREAM_CHUNK_BYTES,
};
use waddle_extensions::runtime::{ExtensionInferenceHost, HostState, LoadedExtension, WasmRuntime};
use waddle_extensions::{DisplayText, ExtensionCapability, ExtensionEvent, PluginId, WaddleId};
use wasmtime::component::Component;
use wasmtime::Store;

struct StaticInferenceHost {
    chunks: Vec<HostInferenceChunk>,
    usage: VerifiedInferenceUsage,
    max_chunk_bytes: usize,
    cancel_rx_slot: Mutex<Option<tokio::sync::watch::Receiver<bool>>>,
}

#[async_trait]
impl ExtensionInferenceHost for StaticInferenceHost {
    async fn start_stream(
        &self,
        _context: &InvocationContext,
        _request: HostInferenceRequest,
    ) -> Result<OpenedInferenceStream, HostInferenceError> {
        let (tx, rx) = mpsc::channel(8);
        let (cancel, cancel_rx) = StreamCancelHandle::new();
        *self.cancel_rx_slot.lock().expect("cancel_rx_slot lock") = Some(cancel_rx);
        let chunks = self.chunks.clone();
        let usage = self.usage.clone();
        tokio::spawn(async move {
            for chunk in chunks {
                if tx.send(StreamItem::Chunk(chunk)).await.is_err() {
                    return;
                }
            }
            let _ = tx.send(StreamItem::EndOfStream(usage)).await;
        });
        Ok(OpenedInferenceStream {
            rx,
            cancel,
            max_chunk_bytes: self.max_chunk_bytes,
        })
    }
}

#[tokio::test]
async fn inference_stream_delivers_first_chunk_and_host_verified_terminal_usage() {
    let (mut stream, tx, cancel_rx, _cancel) =
        InferenceStream::with_bounded_channel(4, DEFAULT_MAX_STREAM_CHUNK_BYTES);

    // Pre-EOF: terminal_usage MUST be None (cannot forge usage mid-stream).
    assert!(stream.terminal_usage().is_none());

    tx.send(StreamItem::Chunk(HostInferenceChunk {
        delta: b"Hello ".to_vec(),
        sequence: 0,
        finish_reason: None,
    }))
    .await
    .expect("send chunk 0");

    let c0 = stream
        .next_chunk()
        .await
        .expect("chunk 0 ok")
        .expect("chunk 0 present");
    assert_eq!(c0.delta, b"Hello ");
    assert_eq!(c0.sequence, 0);
    assert!(
        stream.terminal_usage().is_none(),
        "terminal_usage must remain None before EndOfStream"
    );

    let verified = VerifiedInferenceUsage {
        provider: "openai".to_owned(),
        model: "gpt-4o-mini".to_owned(),
        input_tokens: 42,
        output_tokens: 17,
        cost_microcents: 320,
    };
    tx.send(StreamItem::EndOfStream(verified.clone()))
        .await
        .expect("send eof");

    assert!(stream.next_chunk().await.expect("eof ok").is_none());
    assert_eq!(stream.terminal_usage(), Some(verified));
    assert!(!*cancel_rx.borrow());

    // Dropping post-EOF triggers idempotent cancel without panic.
    drop(stream);
    assert!(*cancel_rx.borrow());
}

#[tokio::test]
async fn inference_stream_through_host_trait_wires_resource_table() {
    let host = StaticInferenceHost {
        chunks: vec![HostInferenceChunk {
            delta: b"tok".to_vec(),
            sequence: 0,
            finish_reason: Some("stop".to_owned()),
        }],
        usage: VerifiedInferenceUsage {
            provider: "anthropic".to_owned(),
            model: "claude-sonnet".to_owned(),
            input_tokens: 10,
            output_tokens: 1,
            cost_microcents: 50,
        },
        max_chunk_bytes: 1024,
        cancel_rx_slot: Mutex::new(None),
    };
    let ctx = InvocationContext {
        tenant_id: None,
        waddle_id: WaddleId::try_new("acme.example").expect("waddle"),
        plugin_id: PluginId::try_new("ai-chatbot").expect("plugin"),
        requester: None,
        source_room: None,
        kind: InvocationKind::Command,
        provider_room_grants: Vec::new(),
    };
    let opened = host
        .start_stream(
            &ctx,
            HostInferenceRequest {
                credential: InferenceCredentialRef::Tenant("default".to_owned()),
                provider: "anthropic".to_owned(),
                deployment: "prod".to_owned(),
                model: "claude-sonnet".to_owned(),
                messages: vec![HostInferenceMessage {
                    role: "user".to_owned(),
                    content: "hi".to_owned(),
                }],
                max_output_tokens: Some(16),
            },
        )
        .await
        .expect("open stream");

    let mut stream = InferenceStream::from_opened(opened);
    let chunk = stream
        .next_chunk()
        .await
        .expect("chunk")
        .expect("some chunk");
    assert_eq!(chunk.delta, b"tok");
    assert!(stream.next_chunk().await.expect("eof").is_none());
    assert_eq!(
        stream
            .terminal_usage()
            .expect("verified usage")
            .output_tokens,
        1
    );
}

#[tokio::test]
async fn stream_drop_cancel_and_drop_boundary_are_sync_and_idempotent() {
    // 1. Drop before first chunk.
    let (stream_pre, _tx1, cancel_rx_1, cancel_1) = InferenceStream::with_bounded_channel(2, 1024);
    assert!(!*cancel_rx_1.borrow());
    drop(stream_pre);
    assert!(
        *cancel_rx_1.borrow(),
        "dropping InferenceStream pre-first-chunk must synchronously cancel upstream"
    );
    cancel_1.cancel_sync();
    assert!(*cancel_rx_1.borrow(), "cancel_sync must be idempotent");

    // 2. Drop mid-stream inside a Store<HostState> dropped by tokio::time::timeout.
    let runtime = WasmRuntime::try_new().expect("runtime");
    let (stream_mid, tx2, cancel_rx_2, _cancel_2) = InferenceStream::with_bounded_channel(2, 1024);
    tx2.send(StreamItem::Chunk(HostInferenceChunk {
        delta: b"first".to_vec(),
        sequence: 0,
        finish_reason: None,
    }))
    .await
    .expect("send first");

    let mut table = wasmtime::component::ResourceTable::new();
    let res = table.push(stream_mid).expect("push resource");
    let mut store = Store::new(runtime.engine(), table);

    // Simulate tokio::time::timeout dropping the Store without calling async WIT drop.
    let timed_out = tokio::time::timeout(Duration::from_millis(5), async move {
        let s = store.data_mut().get_mut(&res).expect("get stream");
        let _first = s.next_chunk().await.expect("first chunk");
        // Never finish: timeout drops `store` (and `ResourceTable` + `InferenceStream`).
        std::future::pending::<()>().await;
    })
    .await;
    assert!(timed_out.is_err());
    assert!(
        *cancel_rx_2.borrow(),
        "dropping Store<ResourceTable> on timeout must synchronously signal upstream cancel"
    );
    assert!(cancel_rx_2.has_changed().is_ok());

    // 3. Drop post-EOF.
    let (mut stream_eof, tx3, cancel_rx_3, _cancel_3) =
        InferenceStream::with_bounded_channel(2, 1024);
    tx3.send(StreamItem::EndOfStream(VerifiedInferenceUsage {
        provider: "openai".to_owned(),
        model: "gpt-4o-mini".to_owned(),
        input_tokens: 1,
        output_tokens: 1,
        cost_microcents: 1,
    }))
    .await
    .expect("eof");
    assert!(stream_eof.next_chunk().await.expect("eof").is_none());
    assert!(!*cancel_rx_3.borrow());
    drop(stream_eof);
    assert!(*cancel_rx_3.borrow());
}

#[tokio::test]
async fn chunk_boundary_enforces_zero_max_and_max_plus_one_bytes() {
    let max = 64usize;
    let (mut stream, tx, cancel_rx, _cancel) = InferenceStream::with_bounded_channel(4, max);

    // 0-byte chunk is accepted.
    tx.send(StreamItem::Chunk(HostInferenceChunk {
        delta: Vec::new(),
        sequence: 0,
        finish_reason: None,
    }))
    .await
    .expect("send 0-byte chunk");
    let c0 = stream
        .next_chunk()
        .await
        .expect("0-byte chunk ok")
        .expect("some");
    assert!(c0.delta.is_empty());

    // Exact max_chunk_bytes (64B) is accepted.
    tx.send(StreamItem::Chunk(HostInferenceChunk {
        delta: vec![b'a'; max],
        sequence: 1,
        finish_reason: None,
    }))
    .await
    .expect("send max chunk");
    let c1 = stream
        .next_chunk()
        .await
        .expect("max chunk ok")
        .expect("some");
    assert_eq!(c1.delta.len(), max);

    // max_chunk_bytes + 1 (65B) is rejected and cancels upstream.
    tx.send(StreamItem::Chunk(HostInferenceChunk {
        delta: vec![b'b'; max + 1],
        sequence: 2,
        finish_reason: None,
    }))
    .await
    .expect("send oversize chunk");
    let err = stream
        .next_chunk()
        .await
        .expect_err("max+1 chunk must fail closed");
    assert_eq!(err.code, HostInferenceErrorCode::ChunkLimitExceeded);
    assert!(
        *cancel_rx.borrow(),
        "oversize chunk must cancel upstream stream"
    );
}

#[tokio::test]
async fn wasi_narrow_boundary_rejects_filesystem_sockets_and_cli_imports() {
    let runtime = WasmRuntime::try_new().expect("runtime");
    let tmp = std::env::temp_dir().join(format!("waddle-narrow-wasi-{}.wat", std::process::id()));
    std::fs::write(&tmp, "(component)").expect("write empty component");
    let loaded = LoadedExtension::load(&runtime, &tmp).expect("load empty component");
    let _ = std::fs::remove_file(&tmp);

    // Verify narrowed Linker<HostState> rejects components importing functions from
    // undeclared `wasi:filesystem`, `wasi:sockets`, or `wasi:cli`.
    for (label, wat) in [
        (
            "wasi:filesystem/preopens@0.2.0",
            r#"(component
                (import "wasi:filesystem/preopens@0.2.0" (instance
                    (export "get-directories" (func (result (list (tuple u32 string)))))
                ))
            )"#,
        ),
        (
            "wasi:sockets/instance-network@0.2.0",
            r#"(component
                (import "wasi:sockets/instance-network@0.2.0" (instance
                    (export "instance-network" (func (result u32)))
                ))
            )"#,
        ),
        (
            "wasi:cli/environment@0.2.0",
            r#"(component
                (import "wasi:cli/environment@0.2.0" (instance
                    (export "get-environment" (func (result (list (tuple string string)))))
                ))
            )"#,
        ),
    ] {
        let component = Component::new(runtime.engine(), wat).expect("compile wat component");
        let mut store = Store::new(runtime.engine(), HostState::default());
        let res = loaded
            .linker()
            .instantiate_async(&mut store, &component)
            .await;
        assert!(
            res.is_err(),
            "narrow WASI linker MUST reject undeclared import {label}"
        );
    }

    // Verify declared WASI interfaces (`wasi:clocks`, `wasi:random`, `wasi:io`) succeed.
    let allowed_wat = r#"(component
        (import "wasi:clocks/monotonic-clock@0.2.0" (instance
            (export "now" (func (result u64)))
        ))
        (import "wasi:random/random@0.2.0" (instance
            (export "get-random-u64" (func (result u64)))
        ))
        (import "wasi:io/poll@0.2.0" (instance
            (export "pollable" (type (sub resource)))
        ))
    )"#;
    let allowed_comp = Component::new(runtime.engine(), allowed_wat).expect("allowed wat");
    let mut store = Store::new(runtime.engine(), HostState::default());
    let inst = loaded
        .linker()
        .instantiate_async(&mut store, &allowed_comp)
        .await;
    assert!(
        inst.is_ok(),
        "narrow WASI linker must still allow clocks/random/io: {inst:?}"
    );
}

#[derive(Clone)]
struct LogCaptureWriter(Arc<Mutex<Vec<u8>>>);

impl io::Write for LogCaptureWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().expect("lock").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogCaptureWriter {
    type Writer = LogCaptureWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test]
async fn stderr_suppression_blocks_guest_logging_and_outbound_http_for_inference() {
    let runtime = WasmRuntime::try_new().expect("runtime");
    let extension = LoadedExtension::load(
        &runtime,
        std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/message_hook.wat"
        )),
    )
    .expect("load fixture");

    let sentinel = "SECRET_PROMPT_SENTINEL_998877";
    let captured = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(LogCaptureWriter(Arc::clone(&captured)))
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    // 1. Inference guest (ai-chatbot) attempting OutboundHttpRequest or allowed_http_origins
    //    is denied before instantiation.
    let err = extension
        .call_handle_event_with_inference(waddle_extensions::runtime::InferenceInvocationParams {
            event: ExtensionEvent::MessageHook(waddle_extensions::types::MessageHook {
                context: waddle_extensions::types::MessageContext {
                    waddle_id: WaddleId::try_new("acme.example").expect("waddle"),
                    stanza_id: None,
                    room: None,
                    sender: None,
                    thread_id: None,
                    reply_to: None,
                },
                body: DisplayText::try_new(sentinel).expect("body"),
                links: Vec::new(),
            }),
            tools: Arc::new(DenyingExtensionHostTools) as Arc<dyn ExtensionHostTools>,
            inference: Arc::new(waddle_extensions::runtime::DenyingExtensionInferenceHost),
            context: InvocationContext {
                tenant_id: None,
                waddle_id: WaddleId::try_new("acme.example").expect("waddle"),
                plugin_id: PluginId::try_new("ai-chatbot").expect("plugin"),
                requester: None,
                source_room: None,
                kind: InvocationKind::Command,
                provider_room_grants: Vec::new(),
            },
            config: "{}".to_owned(),
            grants: HashSet::from([ExtensionCapability::OutboundHttpRequest]),
            allowed_http_origins: vec!["https://api.openai.com".to_owned()],
            tenant_memory_tracker: None,
            inference_invocation: true,
        })
        .await
        .expect_err("inference invocation must reject OutboundHttpRequest");
    assert!(
        err.to_string().contains("Denied"),
        "expected Denied for inference OutboundHttpRequest, got: {err}"
    );

    // 2. Verify HostState suppresses guest logging for inference/Command invocations
    //    via `wasi:logging/logging::Host::log` while keeping non-inference logging intact.
    let mut inf_state = HostState::from_spec(waddle_extensions::runtime::HostStateSpec {
        tools: Arc::new(DenyingExtensionHostTools),
        inference: Arc::new(waddle_extensions::runtime::DenyingExtensionInferenceHost),
        context: InvocationContext {
            tenant_id: None,
            waddle_id: WaddleId::try_new("acme.example").expect("waddle"),
            plugin_id: PluginId::try_new("ai-chatbot").expect("plugin"),
            requester: None,
            source_room: None,
            kind: InvocationKind::Command,
            provider_room_grants: Vec::new(),
        },
        config: "{}".to_owned(),
        grants: HashSet::new(),
        allowed_http_origins: Vec::new(),
        runtime_limits: waddle_extensions::config::RuntimeLimits::default(),
        http: waddle_extensions::runtime::HttpRuntime::try_new().expect("http runtime"),
        tenant_memory_tracker: None,
        inference_invocation: true,
    });
    inf_state.emit_guest_log_for_test("guest", sentinel).await;

    let logs = String::from_utf8(captured.lock().expect("lock").clone()).expect("utf8");
    assert!(
        !logs.contains(sentinel),
        "guest log/stderr sentinel must have 0 occurrences in host logs: {logs}"
    );
}

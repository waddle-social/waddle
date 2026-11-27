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
async fn wasi_narrow_boundary_rejects_filesystem_and_sockets_and_isolates_cli() {
    use wasmtime_wasi::cli::WasiCliView as _;
    use wasmtime_wasi::p2::bindings::cli::environment::Host as _;

    let runtime = WasmRuntime::try_new().expect("runtime");
    let tmp = std::env::temp_dir().join(format!("waddle-narrow-wasi-{}.wat", std::process::id()));
    std::fs::write(&tmp, "(component)").expect("write empty component");
    let loaded = LoadedExtension::load(&runtime, &tmp).expect("load empty component");
    let _ = std::fs::remove_file(&tmp);

    // Verify narrowed Linker<HostState> rejects components importing functions from
    // undeclared `wasi:filesystem` or `wasi:sockets`.
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
            "wasi:sockets/ip-name-lookup@0.2.0",
            r#"(component
                (import "wasi:sockets/ip-name-lookup@0.2.0" (instance
                    (export "resolve-address-stream" (type (sub resource)))
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

    // Verify declared WASI interfaces (`wasi:clocks`, `wasi:random`, `wasi:io`, and
    // isolated `wasi:cli` required by Rust's `wasm32-wasip2` std) succeed.
    let allowed_wat = r#"(component
        (import "wasi:clocks/monotonic-clock@0.2.0" (instance
            (export "now" (func (result u64)))
        ))
        (import "wasi:random/random@0.2.0" (instance
            (export "get-random-u64" (func (result u64)))
        ))
        (import "wasi:random/insecure-seed@0.2.0" (instance
            (export "insecure-seed" (func (result (tuple u64 u64))))
        ))
        (import "wasi:io/poll@0.2.0" (instance
            (export "pollable" (type (sub resource)))
        ))
        (import "wasi:cli/environment@0.2.0" (instance
            (export "get-environment" (func (result (list (tuple string string)))))
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
        "narrow WASI linker must allow clocks/random/io and isolated cli: {inst:?}"
    );

    // Verify `wasi:cli/environment` exposes zero host env vars, args, or cwd.
    let mut state = HostState::default();
    let mut cli = state.cli();
    assert!(
        cli.get_environment().expect("get_environment").is_empty(),
        "guest must never inherit host environment variables"
    );
    assert!(
        cli.get_arguments().expect("get_arguments").is_empty(),
        "guest must never inherit host argv"
    );
    assert!(
        cli.initial_cwd().expect("initial_cwd").is_none(),
        "guest must never inherit host cwd"
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
            chunk_tx: None,
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
        chunk_tx: None,
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

#[tokio::test]
async fn wasm_guest_inference_stream_reads_first_chunk_and_verified_terminal_usage() {
    use tokio::sync::Notify;
    use waddle_extensions::actor::WasmExtensionActor;
    use waddle_extensions::config::RuntimeLimits;
    use waddle_extensions::types::{CommandAction, CommandInvocation, CommandNode, FullJidValue};

    struct ProgressiveInferenceHost {
        first_chunk_seen: Arc<Notify>,
        release_second_chunk: Arc<Notify>,
    }

    #[async_trait]
    impl ExtensionInferenceHost for ProgressiveInferenceHost {
        async fn start_stream(
            &self,
            _context: &InvocationContext,
            request: HostInferenceRequest,
        ) -> Result<OpenedInferenceStream, HostInferenceError> {
            assert_eq!(request.provider, "openai");
            assert_eq!(request.model, "gpt-4o-mini");
            let (tx, rx) = mpsc::channel(4);
            let (cancel, _cancel_rx) = StreamCancelHandle::new();
            let first_chunk_seen = Arc::clone(&self.first_chunk_seen);
            let release_second = Arc::clone(&self.release_second_chunk);
            tokio::spawn(async move {
                let _ = tx
                    .send(StreamItem::Chunk(HostInferenceChunk {
                        delta: b"hello ".to_vec(),
                        sequence: 0,
                        finish_reason: None,
                    }))
                    .await;
                first_chunk_seen.notify_one();
                release_second.notified().await;
                let _ = tx
                    .send(StreamItem::Chunk(HostInferenceChunk {
                        delta: b"world".to_vec(),
                        sequence: 1,
                        finish_reason: Some("stop".to_owned()),
                    }))
                    .await;
                let _ = tx
                    .send(StreamItem::EndOfStream(VerifiedInferenceUsage {
                        provider: "openai".to_owned(),
                        model: "gpt-4o-mini".to_owned(),
                        input_tokens: 12,
                        output_tokens: 2,
                        cost_microcents: 420,
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

    let runtime = WasmRuntime::try_new().expect("runtime");
    let loaded = LoadedExtension::load(
        &runtime,
        std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/inference_stream.wat"
        )),
    )
    .expect("load inference_stream.wat")
    .with_limits(RuntimeLimits::default())
    .expect("limits");

    let first_chunk_seen = Arc::new(Notify::new());
    let release_second_chunk = Arc::new(Notify::new());
    let host = Arc::new(ProgressiveInferenceHost {
        first_chunk_seen: Arc::clone(&first_chunk_seen),
        release_second_chunk: Arc::clone(&release_second_chunk),
    });

    let actor = WasmExtensionActor::initialize(loaded, "{}")
        .await
        .expect("init")
        .with_grants(HashSet::from([ExtensionCapability::Commands]))
        .with_inference_host(host);

    let event = ExtensionEvent::Command(CommandInvocation {
        waddle_id: WaddleId::try_new("acme.example").expect("waddle"),
        room: None,
        requester: FullJidValue::try_new("alice@acme.example/web").expect("requester"),
        command_node: CommandNode::try_new("urn:waddle:extension:1:ai-chatbot").expect("node"),
        session_id: None,
        action: Some(CommandAction::Execute),
        form: None,
        fields: Vec::new(),
    });
    let release_clone = Arc::clone(&release_second_chunk);
    let gate_task = tokio::spawn(async move {
        first_chunk_seen.notified().await;
        // Brief pause verifies guest consumes chunk 0 before stream finishes.
        tokio::time::sleep(Duration::from_millis(30)).await;
        release_clone.notify_one();
    });

    let effects = actor
        .handle_event_for_waddle(event, WaddleId::try_new("acme.example").expect("waddle"))
        .await;
    gate_task.await.expect("gate task");
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        waddle_extensions::ExtensionEffect::HostWarning(text) => {
            assert_eq!(text.as_str(), "hello world");
        }
        other => panic!("expected assembled stream HostWarning effect, got {other:?}"),
    }
}

#[tokio::test]
async fn wasm_guest_inference_stream_drop_cancels_on_epoch_trap_after_first_chunk() {
    use tokio::sync::Notify;
    use waddle_extensions::actor::WasmExtensionActor;
    use waddle_extensions::config::RuntimeLimits;
    use waddle_extensions::types::{
        CommandAction, CommandInvocation, CommandNode, DataFormValue, FormFieldValue, FullJidValue,
        UiActionId,
    };

    struct HangingHost {
        first_chunk_sent: Arc<Notify>,
        cancel_rx_slot: Mutex<Option<tokio::sync::watch::Receiver<bool>>>,
    }

    #[async_trait]
    impl ExtensionInferenceHost for HangingHost {
        async fn start_stream(
            &self,
            _context: &InvocationContext,
            _request: HostInferenceRequest,
        ) -> Result<OpenedInferenceStream, HostInferenceError> {
            let (tx, rx) = mpsc::channel(4);
            let (cancel, cancel_rx) = StreamCancelHandle::new();
            *self.cancel_rx_slot.lock().expect("lock") = Some(cancel_rx);
            let sent = Arc::clone(&self.first_chunk_sent);
            tokio::spawn(async move {
                let _ = tx
                    .send(StreamItem::Chunk(HostInferenceChunk {
                        delta: b"partial".to_vec(),
                        sequence: 0,
                        finish_reason: None,
                    }))
                    .await;
                sent.notify_one();
                std::future::pending::<()>().await;
            });
            Ok(OpenedInferenceStream {
                rx,
                cancel,
                max_chunk_bytes: 1024,
            })
        }
    }

    let runtime = WasmRuntime::try_new().expect("runtime");
    let loaded = LoadedExtension::load(
        &runtime,
        std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/inference_stream.wat"
        )),
    )
    .expect("load inference_stream.wat")
    .with_limits(RuntimeLimits {
        invocation_timeout_ms: 250,
        http_timeout_ms: 250,
        ..RuntimeLimits::default()
    })
    .expect("limits");

    let first_chunk_sent = Arc::new(Notify::new());
    let host = Arc::new(HangingHost {
        first_chunk_sent: Arc::clone(&first_chunk_sent),
        cancel_rx_slot: Mutex::new(None),
    });

    let actor = WasmExtensionActor::initialize(loaded, "{}")
        .await
        .expect("init")
        .with_grants(HashSet::from([ExtensionCapability::Commands]))
        .with_inference_host(Arc::clone(&host) as Arc<dyn ExtensionInferenceHost>);

    let event = ExtensionEvent::Command(CommandInvocation {
        waddle_id: WaddleId::try_new("acme.example").expect("waddle"),
        room: None,
        requester: FullJidValue::try_new("alice@acme.example/web").expect("requester"),
        command_node: CommandNode::try_new("urn:waddle:extension:1:ai-chatbot").expect("node"),
        session_id: None,
        action: Some(CommandAction::Execute),
        form: None,
        fields: vec![FormFieldValue {
            name: UiActionId::try_new("hang").expect("field"),
            values: vec![DataFormValue::new("1")],
        }],
    });

    let effects = actor
        .handle_event_for_waddle(event, WaddleId::try_new("acme.example").expect("waddle"))
        .await;
    // Guest spun after receiving chunk 0 until epoch/deadline trap -> fail-closed denial.
    assert!(
        matches!(
            effects.first(),
            Some(waddle_extensions::ExtensionEffect::HostDenial(_))
        ),
        "expected deadline/epoch trap HostDenial, got {effects:?}"
    );
    let cancel_rx = host
        .cancel_rx_slot
        .lock()
        .expect("lock")
        .take()
        .expect("start_stream was called");
    assert!(
        *cancel_rx.borrow(),
        "dropping Store<HostState> on epoch/deadline trap must synchronously trigger InferenceStream::Drop cancel"
    );
}

#[tokio::test]
async fn denying_inference_host_fails_closed_with_host_denial() {
    use waddle_extensions::actor::WasmExtensionActor;
    use waddle_extensions::config::RuntimeLimits;
    use waddle_extensions::types::{CommandAction, CommandInvocation, CommandNode, FullJidValue};

    let runtime = WasmRuntime::try_new().expect("runtime");
    let loaded = LoadedExtension::load(
        &runtime,
        std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/inference_stream.wat"
        )),
    )
    .expect("load inference_stream.wat")
    .with_limits(RuntimeLimits::default())
    .expect("limits");

    let actor = WasmExtensionActor::initialize(loaded, "{}")
        .await
        .expect("init")
        .with_grants(HashSet::from([ExtensionCapability::Commands]));

    let event = ExtensionEvent::Command(CommandInvocation {
        waddle_id: WaddleId::try_new("acme.example").expect("waddle"),
        room: None,
        requester: FullJidValue::try_new("alice@acme.example/web").expect("requester"),
        command_node: CommandNode::try_new("urn:waddle:extension:1:ai-chatbot").expect("node"),
        session_id: None,
        action: Some(CommandAction::Execute),
        form: None,
        fields: Vec::new(),
    });

    let effects = actor
        .handle_event_for_waddle(event, WaddleId::try_new("acme.example").expect("waddle"))
        .await;
    assert!(
        matches!(
            effects.as_slice(),
            [waddle_extensions::ExtensionEffect::HostDenial(_)]
        ),
        "expected fail-closed HostDenial when DenyingExtensionInferenceHost is active, got {effects:?}"
    );
}

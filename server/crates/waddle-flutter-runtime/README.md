# Flutter native runtime

Independent Flutter clients call this Rust `cdylib` directly. It delegates protocol
operations to the existing `waddle-xmpp-client-ffi` Rust facade and shares the
`waddle-flutter-timeline` state machine with the browser build. It does not depend
on Android/Kotlin application code or expose a raw XML escape hatch.

The C API is declared in `include/waddle_flutter_runtime.h`. Create a handle,
submit `{ "method": "...", "args": { ... } }`, poll owned UTF-8 JSON arrays, and
release every array with `waddle_flutter_free_json`. Submit returns a positive
request ID, or zero if the handle is invalid, the input is too large, or the
request window is full. Unsupported methods and malformed arguments produce an
explicit asynchronous error result. Poll returns at most 128 envelopes.

Result envelopes contain `kind: "result"`, `request_id`, `generation`, `sequence`,
`ok`, and either `data` or `error: { code, message }`. Event envelopes contain
`kind: "event"`, `generation`, `sequence`, and `event: { type, data }`.
RPC results and nested records use snake_case fields. Unit-only enums are
snake_case strings; enums with payloads use `{ "type": "snake_case", ... }`.
See `rpc-schema.json` for the complete existing-client verb surface.

Connect validates credentials/configuration into a fresh client generation.
Resumption is rejected until the app actually persists and reuses stream state.
Disconnect retires the generation and settles outstanding requests. Its result
acknowledges completion of any in-flight connect and confirmed transport closure,
not merely queueing a close command. Lifecycle operations share a serial gate;
a successor cannot open while a retired client is still connecting or closing.
A `close_unconfirmed` error retains the old client for a later shutdown retry.
The Dart owner must retain its handle/poller and refuse takeover until that retry
confirms closure. Native connect is bounded to 15 seconds and close proof to 12;
the Dart shutdown budget includes both. Explicit destroy removes the handle
immediately and is a nonblocking finalizer, not proof of completed shutdown. Old callbacks and completions cannot enter a
successor's queue. Connect cleanup is allowed to finish before its late transport
is closed, avoiding a cancellation gap in the existing client. Ordinary verb jobs
are aborted on retirement. The event queue is bounded; overflow explicitly
requests reconnect/history resynchronization and settles pending futures.

Regenerate checked-in typed dispatch and schema after changing the shared facade:

```sh
python3 scripts/generate_dispatch.py
python3 scripts/generate_dispatch.py --check
```

Generation needs the repository's `rustfmt`. No protocol XML or behavior is
generated. The opt-in `serde` feature on the existing facade adds only JSON I/O
serialization; existing UniFFI clients build with their original feature set.

From `server/`:

```sh
cargo test -p waddle-flutter-runtime
cargo clippy -p waddle-flutter-runtime --all-targets -- -D warnings
cargo build -p waddle-flutter-runtime
python3 crates/waddle-flutter-runtime/scripts/smoke_library.py target/debug/libwaddle_flutter_runtime.so
```

Call results/events may contain media join tokens or attachment encryption
metadata required by protocol clients. Keep them inside the application runtime;
presentation snapshots must omit those fields. Errors never echo configuration,
credential values or arbitrary client diagnostic strings.

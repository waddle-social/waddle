import 'dart:async';
import 'dart:js_interop';

import 'transport.dart';

@JS('waddleRust.create')
external JSPromise<_WasmAdapter> _create(JSFunction emit);

@JS()
extension type _WasmAdapter._(JSObject _) implements JSObject {
  external JSPromise<JSAny?> rpc(JSString method, JSAny? args);
  external JSPromise<JSAny?> close();
}

Future<RustTransport> createTransport() async {
  final events = StreamController<Map<String, dynamic>>.broadcast(sync: true);
  try {
    final adapter = await _create(
      ((JSAny? raw) {
        final value = raw.dartify();
        if (value is Map && !events.isClosed) {
          events.add(Map<String, dynamic>.from(value));
        }
      }).toJS,
    ).toDart;
    return WebRustTransport._(adapter, events);
  } catch (_) {
    await events.close();
    throw const RustTransportException(
      'wasm_loader_missing',
      'The browser Rust loader could not start. Rebuild the standalone web app.',
    );
  }
}

class WebRustTransport implements RustTransport {
  WebRustTransport._(this._adapter, this._events);
  final _WasmAdapter _adapter;
  final StreamController<Map<String, dynamic>> _events;
  bool _closed = false, _closing = false;
  Future<void>? _closeFuture;
  @override
  Stream<Map<String, dynamic>> get events => _events.stream;

  @override
  Future<dynamic> rpc(
    String method, [
    Map<String, dynamic> args = const {},
  ]) async {
    if (_closed || _closing) {
      throw const RustTransportException(
        'closed',
        'The Rust transport is closed.',
      );
    }
    try {
      return (await _adapter.rpc(method.toJS, args.jsify()).toDart).dartify();
    } catch (_) {
      // Do not echo arbitrary JS exception text: it can contain URLs or authentication material.
      throw RustTransportException(
        'wasm_request_failed',
        'The Rust operation "$method" could not be completed.',
      );
    }
  }

  @override
  Future<void> close() {
    if (_closed) {
      return Future.value();
    }
    final running = _closeFuture;
    if (running != null) {
      return running;
    }
    final future = _close();
    _closeFuture = future;
    unawaited(
      future.then<void>(
        (_) {},
        onError: (Object _, StackTrace stack) {
          if (!_closed && identical(_closeFuture, future)) {
            _closeFuture = null;
          }
        },
      ),
    );
    return future;
  }

  Future<void> _close() async {
    _closing = true;
    try {
      await _adapter.close().toDart.timeout(const Duration(seconds: 30));
    } catch (_) {
      throw const RustTransportException(
        'wasm_close_unconfirmed',
        'The browser connection has not finished closing. Retry before reconnecting.',
      );
    }
    _closed = true;
    await _events.close();
  }
}

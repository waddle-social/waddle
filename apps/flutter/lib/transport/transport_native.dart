import 'dart:async';
import 'dart:convert';

import 'native_bindings.dart';
import 'transport.dart';

Future<RustTransport> createTransport() async =>
    NativeRustTransport(FfiRustBindings.load());

class _Request {
  _Request(this.method);
  final String method;
  final completer = Completer<dynamic>();
  Timer? timeout;
}

/// Rust owns Tokio/network tasks. Dart polls only an already serialized, bounded queue.
class NativeRustTransport implements RustTransport {
  NativeRustTransport(
    this.bindings, {
    Duration pollInterval = const Duration(milliseconds: 16),
    this.requestTimeout = const Duration(seconds: 45),
    this.closeTimeout = const Duration(seconds: 30),
    bool startPolling = true,
  }) {
    _handle = bindings.create();
    if (_handle == 0) {
      throw const RustTransportException(
        'create_failed',
        'Could not create the Waddle Rust runtime.',
      );
    }
    if (startPolling) {
      _poller = Timer.periodic(pollInterval, (_) => poll());
    }
  }

  final NativeRustBindings bindings;
  final Duration requestTimeout;
  final Duration closeTimeout;
  late final int _handle;
  final _events = StreamController<Map<String, dynamic>>.broadcast(sync: true);
  final _requests = <int, _Request>{};
  Timer? _poller;
  bool _closing = false, _closed = false, _polling = false;
  int _generation = 0;
  int _sequence = -1;
  Future<void>? _closeFuture;

  @override
  Stream<Map<String, dynamic>> get events => _events.stream;

  @override
  Future<dynamic> rpc(String method, [Map<String, dynamic> args = const {}]) {
    if (_closing || _closed) {
      return Future.error(
        const RustTransportException('closed', 'The Rust transport is closed.'),
      );
    }
    return _submit(method, args);
  }

  Future<dynamic> _submit(String method, Map<String, dynamic> args) {
    final id = bindings.submit(
      _handle,
      jsonEncode({'method': method, 'args': args}),
    );
    if (id <= 0) {
      return Future.error(
        const RustTransportException(
          'request_rejected',
          'The native runtime could not accept this request.',
        ),
      );
    }
    final request = _Request(method);
    _requests[id] = request;
    request.timeout = Timer(requestTimeout, () {
      if (_requests.remove(id) == request && !request.completer.isCompleted) {
        request.completer.completeError(
          const RustTransportException(
            'timeout',
            'The Rust operation timed out.',
          ),
        );
      }
    });
    return request.completer.future;
  }

  /// Public for deterministic contract tests; each poll is nonblocking.
  void poll() {
    if (_closed || _polling) {
      return;
    }
    _polling = true;
    try {
      final batch = jsonDecode(bindings.poll(_handle));
      if (batch is! List) {
        throw const FormatException('Native event queue is not an array');
      }
      for (final raw in batch) {
        if (raw is! Map) {
          continue;
        }
        final envelope = Map<String, dynamic>.from(raw);
        final generation = (envelope['generation'] as num?)?.toInt() ?? 0;
        if (generation > _generation) {
          _generation = generation;
          _sequence = -1;
        }
        if (envelope['kind'] == 'result') {
          final id = (envelope['request_id'] as num?)?.toInt();
          final request = _requests.remove(id);
          if (request == null) {
            continue;
          }
          request.timeout?.cancel();
          if (generation < _generation) {
            request.completer.completeError(
              const RustTransportException(
                'stale_session',
                'The session changed during this operation.',
              ),
            );
          } else if (envelope['ok'] == true) {
            request.completer.complete(envelope['data']);
          } else {
            final error = envelope['error'];
            request.completer.completeError(
              RustTransportException(
                error is Map
                    ? error['code']?.toString() ?? 'rust_error'
                    : 'rust_error',
                error is Map
                    ? error['message']?.toString() ??
                          'The Rust operation failed.'
                    : 'The Rust operation failed.',
              ),
            );
          }
        } else if (envelope['kind'] == 'event' &&
            generation == _generation &&
            !_closing) {
          final sequence = (envelope['sequence'] as num?)?.toInt();
          if (sequence != null && sequence <= _sequence) {
            continue;
          }
          if (sequence != null) {
            _sequence = sequence;
          }
          final event = envelope['event'];
          if (event is Map && event['type'] is String) {
            _events.add(Map<String, dynamic>.from(event));
          }
        }
      }
    } catch (_) {
      const error = RustTransportException(
        'invalid_native_queue',
        'The native event queue could not be read.',
      );
      for (final request in _requests.values) {
        request.timeout?.cancel();
        if (!request.completer.isCompleted) {
          request.completer.completeError(error);
        }
      }
      _requests.clear();
      if (!_closing && _events.hasListener) {
        _events.addError(error);
      }
    } finally {
      _polling = false;
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
    // The native acknowledgment proves driver disconnection, not mere queue admission.
    // On failure retain this handle/poller so callers can retry without creating two owners.
    await _submit('disconnect', const {}).timeout(closeTimeout);
    _closed = true;
    _poller?.cancel();
    try {
      bindings.destroy(_handle);
    } finally {
      for (final request in _requests.values) {
        request.timeout?.cancel();
        if (!request.completer.isCompleted) {
          request.completer.completeError(
            const RustTransportException(
              'closed',
              'The Rust transport is closed.',
            ),
          );
        }
      }
      _requests.clear();
      await _events.close();
    }
  }
}

import 'transport_stub.dart'
    if (dart.library.io) 'transport_native.dart'
    if (dart.library.js_interop) 'transport_web.dart'
    as platform;

/// The protocol boundary. UI state, credentials and reconnect policy belong to Dart.
abstract interface class RustTransport {
  Stream<Map<String, dynamic>> get events;
  Future<dynamic> rpc(String method, [Map<String, dynamic> args = const {}]);

  /// Completes only after Rust has retired connection ownership.
  /// Failure retains this transport and blocks RPC; retry close before creating a successor.
  Future<void> close();
}

Future<RustTransport> createRustTransport() => platform.createTransport();

class RustTransportException implements Exception {
  const RustTransportException(this.code, this.message);
  final String code;
  final String message;
  @override
  String toString() => message;
}

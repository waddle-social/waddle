import 'dart:ffi';
import 'dart:io';

import 'package:ffi/ffi.dart';

import 'transport.dart';

/// The nonblocking JSON C ABI, kept injectable so polling and retirement are testable.
abstract interface class NativeRustBindings {
  int create();
  int submit(int handle, String json);
  String poll(int handle);
  void destroy(int handle);
}

class FfiRustBindings implements NativeRustBindings {
  FfiRustBindings(DynamicLibrary library)
    : _create = library.lookupFunction<Uint64 Function(), int Function()>(
        'waddle_flutter_create',
      ),
      _submit = library
          .lookupFunction<
            Uint64 Function(Uint64, Pointer<Utf8>),
            int Function(int, Pointer<Utf8>)
          >('waddle_flutter_submit'),
      _poll = library
          .lookupFunction<
            Pointer<Utf8> Function(Uint64),
            Pointer<Utf8> Function(int)
          >('waddle_flutter_poll'),
      _free = library
          .lookupFunction<
            Void Function(Pointer<Utf8>),
            void Function(Pointer<Utf8>)
          >('waddle_flutter_free_json'),
      _destroy = library
          .lookupFunction<Void Function(Uint64), void Function(int)>(
            'waddle_flutter_destroy',
          );

  static FfiRustBindings load() {
    const defined = String.fromEnvironment('WADDLE_FLUTTER_RUNTIME_LIBRARY');
    final override = defined.isNotEmpty
        ? defined
        : Platform.environment['WADDLE_FLUTTER_RUNTIME_LIBRARY'];
    if (override != null && override.isNotEmpty) {
      return FfiRustBindings(DynamicLibrary.open(override));
    }
    try {
      return FfiRustBindings(DynamicLibrary.process());
    } on ArgumentError {
      // Android/Linux/Windows bundle the cdylib; Apple may link the symbols into the process.
    }
    final name = Platform.isWindows
        ? 'waddle_flutter_runtime.dll'
        : Platform.isMacOS || Platform.isIOS
        ? 'libwaddle_flutter_runtime.dylib'
        : 'libwaddle_flutter_runtime.so';
    try {
      return FfiRustBindings(DynamicLibrary.open(name));
    } on ArgumentError {
      throw const RustTransportException(
        'rust_library_missing',
        'The Waddle Rust library is missing. Build the native runtime for this platform.',
      );
    }
  }

  final int Function() _create;
  final int Function(int, Pointer<Utf8>) _submit;
  final Pointer<Utf8> Function(int) _poll;
  final void Function(Pointer<Utf8>) _free;
  final void Function(int) _destroy;

  @override
  int create() => _create();

  @override
  int submit(int handle, String json) {
    final input = json.toNativeUtf8();
    try {
      return _submit(handle, input);
    } finally {
      malloc.free(input);
    }
  }

  @override
  String poll(int handle) {
    final result = _poll(handle);
    if (result == nullptr) {
      throw const RustTransportException(
        'poll_failed',
        'The native runtime could not return its event queue.',
      );
    }
    try {
      return result.toDartString();
    } finally {
      _free(result);
    }
  }

  @override
  void destroy(int handle) => _destroy(handle);
}

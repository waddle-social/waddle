import 'package:flutter/foundation.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';

import 'storage.dart';

/// This application's credential store. It never opens the Kotlin app's files,
/// preference names, keychain groups or account database.
class SecureRuntimeStorage implements RuntimeStorage {
  const SecureRuntimeStorage({FlutterSecureStorage? storage})
    : _storage = storage ?? const FlutterSecureStorage();

  final FlutterSecureStorage _storage;
  static const _prefix = 'social.waddle.flutter.';
  // Each browser JS runtime gets a fresh resource. localStorage/sessionStorage
  // can be shared or cloned by another tab and must not select its XMPP resource.
  static String? _tabResource;

  @override
  Future<String?> read(String key) {
    if (kIsWeb && key == 'resource') return Future.value(_tabResource);
    return _storage.read(key: '$_prefix$key');
  }

  @override
  Future<void> write(String key, String? value) {
    if (kIsWeb && key == 'resource') {
      _tabResource = value;
      return Future.value();
    }
    return value == null
        ? _storage.delete(key: '$_prefix$key')
        : _storage.write(key: '$_prefix$key', value: value);
  }
}

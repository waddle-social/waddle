import 'dart:async';
import 'dart:js_interop';

import 'notification_backend.dart';

@JS('waddlePush.initialize')
external JSPromise<JSAny?> _initialize(JSFunction emit);
@JS('waddlePush.invoke')
external JSPromise<JSAny?> _invoke(JSString method, JSAny? args);

NotificationBackend createBackend() => BrowserNotifications();
Future<NotificationBackend> initializeForwarder() async => createBackend();

class BrowserNotifications implements NotificationBackend {
  bool _supported = false, _disposed = false;
  @override
  bool get supported => _supported;
  @override
  bool get supportsBackground => false;
  Future<dynamic> _call(
    String method, [
    Map<String, dynamic> args = const {},
  ]) async => (await _invoke(method.toJS, args.jsify()).toDart).dartify();
  @override
  Future<void> initialize(void Function(NotificationPlatformEvent) emit) async {
    try {
      final result = await _initialize(
        ((JSAny? raw) {
          final value = raw.dartify();
          if (!_disposed && value is Map) {
            emit(Map<String, dynamic>.from(value));
          }
        }).toJS,
      ).toDart;
      _supported = result.dartify() == true;
    } catch (_) {
      _supported = false;
    }
  }

  @override
  Future<bool> requestPermission() async =>
      _supported && await _call('permission') == true;
  @override
  Future<bool> ensurePermission({bool requestOnce = false}) async =>
      _supported && await _call('permissionStatus') == true;
  @override
  Future<void> account(
    String? owner,
    String? instance, {
    required bool enabled,
    required bool visible,
    bool sound = true,
  }) async {
    if (_supported) {
      await _call('account', {
        'owner': owner,
        'instance': instance,
        'enabled': enabled,
        'visible': visible,
        'sound': sound,
      });
    }
  }

  @override
  Future<List<String>> distributors() async =>
      _supported ? ['Browser Web Push'] : [];
  @override
  Future<String?> distributor() async => _supported ? 'Browser Web Push' : null;
  @override
  Future<void> selectDistributor(String value) async {}
  @override
  Future<void> subscribe(String instance, String vapid) async {
    if (_supported) {
      await _call('subscribe', {'instance': instance, 'vapid': vapid});
    }
  }

  @override
  Future<void> unsubscribe(String instance) async {
    if (_supported) await _call('unsubscribe', {'instance': instance});
  }

  @override
  Future<void> show(NotificationNotice notice) async {
    if (_supported) {
      await _call('show', {
        'id': notice.id,
        'title': notice.title,
        'body': notice.body,
        'payload': notice.payload,
        'sound': notice.sound,
      });
    }
  }

  @override
  Future<void> clear() async {
    if (_supported) await _call('clear');
  }

  @override
  Future<void> cancel(int id) async {
    if (_supported) await _call('cancel', {'id': id});
  }

  @override
  Future<void> settings() async {
    throw UnsupportedError('Use your browser’s site notification settings.');
  }

  @override
  Future<bool> background(
    bool enabled, {
    bool requestPermission = false,
  }) async => false;
  @override
  Future<void> dispose() async {
    _disposed = true;
    if (_supported) await _call('detach');
  }

  @override
  Future<void> callForeground(bool active, {bool video = false}) async {}
}

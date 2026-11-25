import 'package:flutter/services.dart';
import 'package:livekit_client/livekit_client.dart' as lk;

/// The OS can expand or dismiss a compact call without a Flutter button press.
class CallWindow {
  CallWindow(this.changed);
  final void Function() changed;
  static const _channel = MethodChannel('social.waddle.flutter/call_window');
  bool available = false, compact = false;
  bool _listening = false;
  Future<void> _tail = Future.value();

  void _update(bool value) {
    if (compact == value) return;
    compact = value;
    changed();
  }

  Future<void> _queue(Future<void> Function() task) {
    final next = _tail.then((_) => task());
    _tail = next.catchError((Object _) {});
    return next;
  }

  Future<void> active(bool enabled) => _queue(() async {
    if (!lk.lkPlatformIs(lk.PlatformType.android) &&
        !lk.lkPlatformIs(lk.PlatformType.linux)) {
      return;
    }
    if (!_listening && !enabled) return;
    if (!_listening) {
      _channel.setMethodCallHandler((event) async {
        if (event.method == 'changed' && event.arguments is bool) {
          _update(event.arguments as bool);
        }
      });
      _listening = true;
    }
    try {
      await _channel.invokeMethod<void>('setActive', enabled);
      _update(await _channel.invokeMethod<bool>('state') == true);
      available = enabled;
    } on MissingPluginException {
      available = false;
    } on PlatformException {
      available = false;
    } finally {
      if (!enabled || !available) {
        _channel.setMethodCallHandler(null);
        _listening = false;
        _update(false);
      }
      changed();
    }
  });

  Future<void> setCompact(bool enabled) => _queue(() async {
    if (!available) throw StateError('Compact calls are unavailable.');
    final actual = await _channel.invokeMethod<bool>('setCompact', enabled);
    _update(actual == true);
    if (actual != enabled) {
      throw StateError('The window manager declined the change.');
    }
  });
}

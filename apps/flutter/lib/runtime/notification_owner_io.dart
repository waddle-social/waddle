import 'dart:async';
import 'dart:isolate';
import 'dart:ui';

import 'package:flutter/widgets.dart';
import 'package:shared_preferences/shared_preferences.dart';

typedef NotificationBootstrap =
    Future<void> Function(Map<String, dynamic> event);

/// One account runtime per Android process, including plugin-created engines.
/// A UI takeover waits for the headless owner to flush and disconnect before
/// registering a successor. A timeout fails closed instead of opening two stores.
class NotificationOwner {
  static const _name = 'social.waddle.flutter.account-owner';
  static const _bootstrapKey = 'waddle.flutter.notification-bootstrap';
  static ReceivePort? _port;
  static Future<void> _tail = Future<void>.value();
  static bool _retiring = false;

  static Future<bool> acquire({
    required bool headless,
    required Future<void> Function(Map<String, dynamic>) onEvent,
    required Future<void> Function() onRetire,
  }) async {
    if (_port != null) return false;
    for (var attempt = 0; attempt < 3; attempt++) {
      final existing = IsolateNameServer.lookupPortByName(_name);
      if (existing != null) {
        if (headless) return false;
        final response = ReceivePort();
        try {
          existing.send({'type': 'takeover', 'reply': response.sendPort});
          final allowed = await response.first.timeout(
            const Duration(seconds: 45),
          );
          if (allowed != true) return false;
        } finally {
          response.close();
        }
      }
      final port = ReceivePort();
      if (!IsolateNameServer.registerPortWithName(port.sendPort, _name)) {
        port.close();
        continue;
      }
      _port = port;
      _retiring = false;
      port.listen((dynamic raw) {
        if (raw is! Map) return;
        final reply = raw['reply'];
        if (reply is! SendPort) return;
        if (_retiring) {
          reply.send('retiring');
          return;
        }
        if (raw['type'] == 'takeover') {
          if (!headless) {
            reply.send(false);
            return;
          }
          _retiring = true;
          _tail = _tail.catchError((Object _) {}).then((_) async {
            try {
              await onRetire();
              release();
              reply.send(true);
            } catch (_) {
              // Retain ownership on a failed drain: no parallel session.
              _retiring = false;
              reply.send(false);
            }
          });
        } else {
          final event = raw['event'];
          _tail = _tail.catchError((Object _) {}).then((_) async {
            try {
              if (event is Map) await onEvent(Map<String, dynamic>.from(event));
              reply.send(true);
            } catch (_) {
              reply.send(false);
            }
          });
        }
      });
      return true;
    }
    return false;
  }

  static Future<void> configureBootstrap(NotificationBootstrap callback) async {
    final handle = PluginUtilities.getCallbackHandle(callback);
    if (handle == null) {
      throw ArgumentError(
        'Notification bootstrap must be a top-level entrypoint.',
      );
    }
    await SharedPreferencesAsync().setInt(_bootstrapKey, handle.toRawHandle());
  }

  static Future<bool> deliver(Map<String, dynamic> event) async {
    final deadline = DateTime.now().add(const Duration(seconds: 45));
    var handoff = false;
    while (DateTime.now().isBefore(deadline)) {
      final owner = IsolateNameServer.lookupPortByName(_name);
      if (owner == null) {
        if (!handoff) break;
        await Future<void>.delayed(const Duration(milliseconds: 30));
        continue;
      }
      final response = ReceivePort();
      try {
        owner.send({
          'type': 'event',
          'event': event,
          'reply': response.sendPort,
        });
        final result = await response.first.timeout(
          deadline.difference(DateTime.now()),
        );
        // Retry only an explicitly unexecuted event during ownership handoff.
        // A generic failure may follow a durable send and must not replay it.
        if (result != 'retiring') return result == true;
        handoff = true;
      } finally {
        response.close();
      }
      await Future<void>.delayed(const Duration(milliseconds: 30));
    }
    if (handoff) return false;
    // The callback handle contains no credential. The root bootstrap must load
    // and validate the secure account before restoring a Rust connection.
    WidgetsFlutterBinding.ensureInitialized();
    DartPluginRegistrant.ensureInitialized();
    final raw = await SharedPreferencesAsync().getInt(_bootstrapKey);
    if (raw == null) return false;
    final callback = PluginUtilities.getCallbackFromHandle(
      CallbackHandle.fromRawHandle(raw),
    );
    if (callback is! NotificationBootstrap) return false;
    await callback(event);
    return true;
  }

  static void release() {
    final port = _port;
    if (port == null) return;
    if (IsolateNameServer.lookupPortByName(_name) == port.sendPort) {
      IsolateNameServer.removePortNameMapping(_name);
    }
    port.close();
    _port = null;
  }
}

import 'dart:convert';

import 'package:flutter_local_notifications/flutter_local_notifications.dart';

import 'notification_backend.dart';
import 'notification_backend_stub.dart';

/// Desktop notifications are delivered by the active XMPP connection.
class LinuxNotifications extends UnsupportedNotifications {
  final _plugin = FlutterLocalNotificationsPlugin();
  bool _initialized = false;

  @override
  bool get localDeliveryOnly => true;

  @override
  Future<void> initialize(void Function(NotificationPlatformEvent) emit) async {
    try {
      _initialized =
          await _plugin.initialize(
            settings: const InitializationSettings(
              linux: LinuxInitializationSettings(defaultActionName: 'Open'),
            ),
            onDidReceiveNotificationResponse: (response) {
              final data = response.payload;
              if (data == null) return;
              final dynamic payload;
              try {
                payload = jsonDecode(data);
              } on FormatException {
                return;
              }
              if (payload is! Map) return;
              emit({
                'type': 'action',
                'action': response.actionId ?? 'open',
                'payload': payload,
              });
            },
          ) ==
          true;
    } on Exception {
      // Minimal desktops can run Waddle without a notification daemon.
      _initialized = false;
    }
  }

  @override
  Future<bool> ensurePermission({bool requestOnce = false}) async {
    if (!_initialized) return false;
    try {
      final linux = _plugin
          .resolvePlatformSpecificImplementation<
            LinuxFlutterLocalNotificationsPlugin
          >();
      if (linux == null) return false;
      await linux.getCapabilities();
      return true;
    } on Exception {
      return false;
    }
  }

  @override
  Future<void> show(NotificationNotice notice) async {
    if (!_initialized) return;
    await _plugin.show(
      id: notice.id,
      title: notice.title,
      body: notice.body,
      payload: jsonEncode(notice.payload),
      notificationDetails: NotificationDetails(
        linux: LinuxNotificationDetails(
          suppressSound: !notice.sound,
          urgency: notice.incomingCall
              ? LinuxNotificationUrgency.critical
              : LinuxNotificationUrgency.normal,
          actions: notice.incomingCall
              ? const [
                  LinuxNotificationAction(key: 'answer', label: 'Answer'),
                  LinuxNotificationAction(key: 'decline', label: 'Decline'),
                ]
              : const [],
        ),
      ),
    );
  }

  @override
  Future<void> cancel(int id) async {
    if (!_initialized) return;
    await _plugin.cancel(id: id);
  }

  @override
  Future<void> clear() async {
    if (!_initialized) return;
    final linux = _plugin
        .resolvePlatformSpecificImplementation<
          LinuxFlutterLocalNotificationsPlugin
        >();
    final ids = await linux?.getSystemIdMap();
    for (final id in ids?.keys ?? const <int>[]) {
      if (id >= 0x10000000) await cancel(id);
    }
  }
}

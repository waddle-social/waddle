import 'dart:async';

import 'package:dbus/dbus.dart';
import 'package:flutter/services.dart';

import 'notification_backend.dart';
import 'notification_backend_stub.dart';

/// The desktop sends activation tokens and actions to the same bus connection
/// that posted the notification. Keep each token with its original notice.
class LinuxNotifications extends UnsupportedNotifications
    implements DesktopNotificationPresentation {
  LinuxNotifications({DBusClient Function()? connect})
    : _connect = connect ?? DBusClient.session;

  static const _name = 'org.freedesktop.Notifications';
  static final _path = DBusObjectPath('/org/freedesktop/Notifications');
  static const _window = MethodChannel('social.waddle.flutter/window');
  final DBusClient Function() _connect;
  DBusClient? _bus;
  DBusRemoteObject? _daemon;
  StreamSubscription<DBusSignal>? _signals;
  StreamSubscription<DBusNameOwnerChangedEvent>? _ownerChanges;
  final _notices = <int, NotificationNotice>{};
  final _tokens = <int, String>{};
  Future<void> _tail = Future.value();

  @override
  bool get localDeliveryOnly => true;

  @override
  Future<void> initialize(void Function(NotificationPlatformEvent) emit) async {
    try {
      final bus = _bus = _connect();
      await bus.getId();
      _daemon = DBusRemoteObject(bus, name: _name, path: _path);
      _ownerChanges = bus.nameOwnerChanged.listen((event) {
        if (event.name == _name && event.oldOwner != null) {
          _notices.clear();
          _tokens.clear();
        }
      });
      _signals =
          DBusSignalStream(
            bus,
            sender: _name,
            interface: _name,
            path: _path,
          ).listen((signal) {
            if (signal.values.length != 2 ||
                signal.values.first is! DBusUint32) {
              return;
            }
            final id = signal.values.first.asUint32();
            final notice = _notices[id];
            if (notice == null) return;
            if (signal.name == 'NotificationClosed') {
              _notices.remove(id);
              _tokens.remove(id);
              return;
            }
            if (signal.values.last is! DBusString) return;
            final value = signal.values.last.asString();
            if (signal.name == 'ActivationToken') {
              _tokens[id] = value;
            } else if (signal.name == 'ActionInvoked') {
              final token = _tokens.remove(id);
              if (value != 'default' &&
                  !(notice.incomingCall
                          ? const {'answer', 'decline'}
                          : const {'compose', 'markRead'})
                      .contains(value)) {
                return;
              }
              emit({
                'type': 'action',
                'action': value == 'default' ? 'open' : value,
                'payload': notice.payload,
                'activationToken': ?token,
              });
            }
          });
    } on Exception {
      await dispose();
    }
  }

  @override
  Future<bool> ensurePermission({bool requestOnce = false}) async {
    try {
      return await _daemon?.callMethod(
            _name,
            'GetCapabilities',
            [],
            replySignature: DBusSignature('as'),
          ) !=
          null;
    } on Exception {
      return false;
    }
  }

  Future<void> _serial(Future<void> Function() operation) {
    final next = _tail.then((_) => operation());
    _tail = next.catchError((Object _) {});
    return next;
  }

  int? _systemId(int id) {
    for (final entry in _notices.entries) {
      if (entry.value.id == id) return entry.key;
    }
    return null;
  }

  @override
  Future<void> show(NotificationNotice notice) => _serial(() async {
    final daemon = _daemon;
    if (daemon == null) return;
    final result = await daemon.callMethod(_name, 'Notify', [
      const DBusString('Waddle'),
      DBusUint32(_systemId(notice.id) ?? 0),
      const DBusString(''),
      DBusString(notice.title),
      DBusString(notice.body),
      DBusArray.string([
        'default',
        'Open',
        if (notice.incomingCall) ...[
          'answer',
          'Answer',
          'decline',
          'Decline',
        ] else ...[
          'compose',
          'Reply',
          'markRead',
          'Mark as read',
        ],
      ]),
      DBusDict.stringVariant({
        'urgency': DBusByte(notice.incomingCall ? 2 : 1),
        'suppress-sound': DBusBoolean(!notice.sound),
        'transient': const DBusBoolean(true),
      }),
      const DBusInt32(-1),
    ], replySignature: DBusSignature('u'));
    final id = result.returnValues.single.asUint32();
    _tokens.remove(id);
    _notices[id] = notice;
  });

  @override
  Future<void> present(String? token) => _window.invokeMethod('present', token);

  Future<void> _close(int id) async {
    _notices.remove(id);
    _tokens.remove(id);
    await _daemon?.callMethod(_name, 'CloseNotification', [
      DBusUint32(id),
    ], replySignature: DBusSignature(''));
  }

  @override
  Future<void> cancel(int id) => _serial(() async {
    final systemId = _systemId(id);
    if (systemId != null) await _close(systemId);
  });

  @override
  Future<void> clear() => _serial(() async {
    final ids = _notices.entries
        .where((entry) => entry.value.id >= 0x10000000)
        .map((entry) => entry.key)
        .toList();
    for (final id in ids) {
      await _close(id);
    }
  });

  @override
  Future<void> dispose() async {
    await _tail;
    await _signals?.cancel();
    await _ownerChanges?.cancel();
    await _bus?.close();
    _signals = null;
    _ownerChanges = null;
    _daemon = null;
    _bus = null;
    _notices.clear();
    _tokens.clear();
  }
}

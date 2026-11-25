import 'dart:async';
import 'dart:io';

import 'package:dbus/dbus.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_flutter/runtime/notification_backend.dart';
import 'package:waddle_flutter/runtime/notification_backend_linux.dart';

const service = 'org.freedesktop.Notifications';

class _Daemon extends DBusObject {
  _Daemon() : super(DBusObjectPath('/org/freedesktop/Notifications'));
  String? destination;
  List<String> actions = [];
  final closed = <int>[];

  @override
  Future<DBusMethodResponse> handleMethodCall(DBusMethodCall call) async {
    switch (call.name) {
      case 'Notify':
        destination = call.sender;
        actions = call.values[5].asStringArray().toList();
        return DBusMethodSuccessResponse([const DBusUint32(7)]);
      case 'CloseNotification':
        closed.add(call.values.single.asUint32());
        return DBusMethodSuccessResponse();
      default:
        return DBusMethodErrorResponse.unknownMethod();
    }
  }

  Future<void> signal(String name, String value) => client!.emitSignal(
    destination: destination,
    path: path,
    interface: service,
    name: name,
    values: [const DBusUint32(7), DBusString(value)],
  );
}

void main() {
  test(
    'the notification daemon delivers one activation token to its selected action',
    () async {
      final server = DBusServer();
      final address = await server.listenAddress(
        DBusAddress.unix(dir: Directory.systemTemp),
      );
      final daemon = DBusClient(address);
      final backend = LinuxNotifications(connect: () => DBusClient(address));
      final events = StreamController<NotificationPlatformEvent>.broadcast();
      try {
        final object = _Daemon();
        await daemon.registerObject(object);
        await daemon.requestName(service);
        await backend.initialize(events.add);
        const notice = NotificationNotice(
          id: 37,
          title: 'Room',
          body: 'New message',
          payload: {'owner': 'alice'},
        );
        await backend.show(notice);
        expect(object.actions, [
          'default',
          'Open',
          'compose',
          'Reply',
          'markRead',
          'Mark as read',
        ]);
        final reply = events.stream.first;
        await object.signal('ActivationToken', 'compositor-token');
        await object.signal('ActionInvoked', 'compose');
        expect(await reply.timeout(const Duration(seconds: 3)), {
          'type': 'action',
          'action': 'compose',
          'payload': notice.payload,
          'activationToken': 'compositor-token',
        });
        final open = events.stream.first;
        await object.signal('ActionInvoked', 'default');
        expect(await open.timeout(const Duration(seconds: 3)), {
          'type': 'action',
          'action': 'open',
          'payload': notice.payload,
        });
        await backend.cancel(notice.id);
        expect(object.closed, [7]);
      } finally {
        await backend.dispose();
        await daemon.close();
        await server.close();
        await events.close();
      }
    },
    skip: !Platform.isLinux,
  );
}

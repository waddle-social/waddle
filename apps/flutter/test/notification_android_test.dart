import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:flutter_local_notifications/flutter_local_notifications.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:permission_handler/permission_handler.dart';
import 'package:waddle_flutter/runtime/notification_backend.dart';
import 'package:waddle_flutter/runtime/notification_backend_io.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  const notifications = MethodChannel(
    'dexterous.com/flutter/local_notifications',
  );
  const permissions = MethodChannel('flutter.baseflow.com/permissions/methods');
  final calls = <MethodCall>[];
  final granted = <int>{};
  setUp(() {
    calls.clear();
    granted.clear();
    debugDefaultTargetPlatformOverride = TargetPlatform.android;
    AndroidFlutterLocalNotificationsPlugin.registerWith();
    final messenger =
        TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
    messenger.setMockMethodCallHandler(notifications, (call) async {
      calls.add(call);
      return true;
    });
    messenger.setMockMethodCallHandler(permissions, (call) async {
      expect(
        call.method,
        'checkPermissionStatus',
        reason: 'Foreground hook never requests capture permissions',
      );
      return granted.contains(call.arguments) ? 1 : 0;
    });
  });
  tearDown(() {
    debugDefaultTargetPlatformOverride = null;
    final messenger =
        TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
    messenger.setMockMethodCallHandler(notifications, null);
    messenger.setMockMethodCallHandler(permissions, null);
  });
  test(
    'call foreground types reflect granted capture and never use manifest-all',
    () async {
      final backend = AndroidNotifications();
      granted.add(Permission.microphone.value);
      await backend.callForeground(true, video: true);
      final args = Map<String, dynamic>.from(calls.single.arguments as Map);
      expect(args['foregroundServiceTypes'], unorderedEquals([512, 128]));
      expect(args['foregroundServiceTypes'], isNot(contains(-1)));
      await backend.callForeground(false);
      expect(calls.last.method, 'stopForegroundService');
    },
  );
  test(
    'receive-only call requires no microphone or camera foreground type',
    () async {
      final backend = AndroidNotifications();
      await backend.callForeground(true, video: true);
      final args = Map<String, dynamic>.from(calls.single.arguments as Map);
      expect(args['foregroundServiceTypes'], [512]);
      await backend.callForeground(false);
    },
  );
  test(
    'incoming call has full-screen fallback and explicit UI/headless actions',
    () async {
      final backend = AndroidNotifications();
      await backend.show(
        const NotificationNotice(
          id: 9003,
          title: 'Alex',
          body: 'Incoming voice call',
          payload: {'kind': 'call', 'sid': 'call-A'},
          incomingCall: true,
        ),
      );
      final args = Map<String, dynamic>.from(calls.single.arguments as Map);
      final details = Map<String, dynamic>.from(
        args['platformSpecifics'] as Map,
      );
      expect(details['fullScreenIntent'], isTrue);
      expect(details['category'], 'call');
      final actions = (details['actions'] as List).cast<Map>();
      expect(actions.first['showsUserInterface'], isTrue);
      expect(actions.last['showsUserInterface'], isFalse);
      expect(actions.map((action) => action['id']), [
        'call_answer',
        'call_decline',
      ]);
    },
  );
}

import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:ui';

import 'package:flutter/widgets.dart';
import 'package:flutter_background/flutter_background.dart';
import 'package:flutter_local_notifications/flutter_local_notifications.dart';
import 'package:permission_handler/permission_handler.dart';
import 'package:shared_preferences/shared_preferences.dart';
import 'package:unifiedpush/unifiedpush.dart';

import 'notification_backend.dart';
import 'notification_backend_stub.dart';
import 'notification_owner_io.dart';

NotificationBackend createBackend() =>
    Platform.isAndroid ? AndroidNotifications() : UnsupportedNotifications();

Future<NotificationBackend> initializeForwarder() async {
  final backend = createBackend();
  await backend.initialize(
    (event) => unawaited(NotificationOwner.deliver(event)),
  );
  return backend;
}

Map<String, dynamic> _response(NotificationResponse response) {
  Map<String, dynamic> payload = {};
  try {
    final value = jsonDecode(response.payload ?? 'null');
    if (value is Map) payload = Map<String, dynamic>.from(value);
  } catch (_) {
    /* Malformed or old notification payloads are ignored by the owner. */
  }
  return {
    'type': 'action',
    'action': switch (response.actionId) {
      'reply' => 'reply',
      'call_answer' => 'answer',
      'call_decline' => 'decline',
      _ => 'open',
    },
    'input': response.input,
    'payload': payload,
  };
}

@pragma('vm:entry-point')
void notificationResponseBackground(NotificationResponse response) async {
  WidgetsFlutterBinding.ensureInitialized();
  DartPluginRegistrant.ensureInitialized();
  await NotificationOwner.deliver(_response(response));
}

class AndroidNotifications implements NotificationBackend {
  final _notifications = FlutterLocalNotificationsPlugin();
  bool _disposed = false;
  bool _callStarted = false;
  @override
  bool get supported => true;
  @override
  bool get supportsBackground => true;
  @override
  Future<void> account(
    String? owner,
    String? instance, {
    required bool enabled,
    required bool visible,
    bool sound = true,
  }) async {}

  @override
  Future<void> initialize(void Function(NotificationPlatformEvent) emit) async {
    void publish(Map<String, dynamic> event) {
      if (!_disposed) emit(event);
    }

    await _notifications.initialize(
      settings: const InitializationSettings(
        android: AndroidInitializationSettings('@mipmap/ic_launcher'),
      ),
      onDidReceiveNotificationResponse: (response) =>
          publish(_response(response)),
      onDidReceiveBackgroundNotificationResponse:
          notificationResponseBackground,
    );
    await UnifiedPush.initialize(
      onNewEndpoint: (endpoint, instance) {
        final keys = endpoint.pubKeySet;
        if (keys != null) {
          publish({
            'type': 'endpoint',
            'instance': instance,
            'endpoint': {
              'endpoint': endpoint.url,
              'p256dh': keys.pubKey,
              'auth': keys.auth,
            },
          });
        }
      },
      onMessage: (message, instance) => publish({
        'type': 'push',
        'instance': instance,
        'decrypted': message.decrypted,
        'content': message.content,
      }),
      onUnregistered: (instance) =>
          publish({'type': 'unregistered', 'instance': instance}),
      onRegistrationFailed: (_, instance) =>
          publish({'type': 'registrationFailed', 'instance': instance}),
      onTempUnavailable: (instance) =>
          publish({'type': 'registrationFailed', 'instance': instance}),
    );
    final launch = await _notifications.getNotificationAppLaunchDetails();
    if (launch?.didNotificationLaunchApp == true &&
        launch?.notificationResponse != null) {
      publish(_response(launch!.notificationResponse!));
    }
  }

  @override
  Future<bool> requestPermission() async {
    await SharedPreferencesAsync().setBool(
      'waddle.flutter.notification-permission-prompted',
      true,
    );
    return await _notifications
            .resolvePlatformSpecificImplementation<
              AndroidFlutterLocalNotificationsPlugin
            >()
            ?.requestNotificationsPermission() ==
        true;
  }

  @override
  Future<bool> ensurePermission({bool requestOnce = false}) async {
    final allowed =
        await _notifications
            .resolvePlatformSpecificImplementation<
              AndroidFlutterLocalNotificationsPlugin
            >()
            ?.areNotificationsEnabled() ==
        true;
    if (!requestOnce) return allowed;
    final preferences = SharedPreferencesAsync();
    if (await preferences.getBool(
          'waddle.flutter.notification-permission-prompted',
        ) ==
        true) {
      return allowed;
    }
    await preferences.setBool(
      'waddle.flutter.notification-permission-prompted',
      true,
    );
    return allowed || await requestPermission();
  }

  @override
  Future<List<String>> distributors() => UnifiedPush.getDistributors();
  @override
  Future<String?> distributor() => UnifiedPush.getDistributor();
  @override
  Future<void> selectDistributor(String value) =>
      UnifiedPush.saveDistributor(value);
  @override
  Future<void> subscribe(String instance, String vapid) =>
      UnifiedPush.register(instance: instance, vapid: vapid);
  @override
  Future<void> unsubscribe(String instance) => UnifiedPush.unregister(instance);
  @override
  Future<void> show(NotificationNotice notice) => _notifications.show(
    id: notice.id,
    title: notice.title,
    body: notice.body,
    payload: jsonEncode(notice.payload),
    notificationDetails: NotificationDetails(
      android: notice.incomingCall
          ? const AndroidNotificationDetails(
              'waddle-flutter-incoming-calls',
              'Incoming calls',
              channelDescription: 'Incoming voice and video calls',
              importance: Importance.max,
              priority: Priority.max,
              category: AndroidNotificationCategory.call,
              fullScreenIntent: true,
              ongoing: true,
              autoCancel: false,
              audioAttributesUsage: AudioAttributesUsage.notificationRingtone,
              sound: UriAndroidNotificationSound(
                'content://settings/system/ringtone',
              ),
              visibility: NotificationVisibility.private,
              actions: [
                AndroidNotificationAction(
                  'call_answer',
                  'Answer',
                  showsUserInterface: true,
                ),
                AndroidNotificationAction('call_decline', 'Decline'),
              ],
            )
          : AndroidNotificationDetails(
              notice.sound
                  ? 'waddle-flutter-messages'
                  : 'waddle-flutter-messages-quiet',
              notice.sound ? 'Messages' : 'Messages without sound',
              channelDescription:
                  'Private, content-free conversation summaries',
              importance: Importance.high,
              priority: Priority.high,
              playSound: notice.sound,
              enableVibration: notice.sound,
              visibility: NotificationVisibility.private,
              actions: const [
                AndroidNotificationAction(
                  'reply',
                  'Reply',
                  inputs: [AndroidNotificationActionInput(label: 'Message')],
                  semanticAction: SemanticAction.reply,
                ),
              ],
            ),
    ),
  );
  @override
  Future<void> clear() async {
    // Message visibility changes must not cancel an incoming call or another
    // service's ongoing notification. Message IDs occupy their own range.
    for (final notification in await _notifications.getActiveNotifications()) {
      final id = notification.id;
      if (id != null && id >= 0x10000000) {
        await _notifications.cancel(id: id);
      }
    }
  }

  @override
  Future<void> cancel(int id) => _notifications.cancel(id: id);
  @override
  Future<void> settings() async {
    await openAppSettings();
  }

  @override
  Future<bool> background(
    bool enabled, {
    bool requestPermission = false,
  }) async {
    if (!enabled) {
      if (FlutterBackground.isBackgroundExecutionEnabled) {
        await FlutterBackground.disableBackgroundExecution();
      }
      return false;
    }
    if (FlutterBackground.isBackgroundExecutionEnabled) return true;
    if (!requestPermission && !await FlutterBackground.hasPermissions) {
      return false;
    }
    final initialized = await FlutterBackground.initialize(
      androidConfig: FlutterBackgroundAndroidConfig(
        notificationTitle: 'Waddle is connected',
        notificationText: 'Receiving messages for the independent Flutter app',
        notificationIcon: const AndroidResource(
          name: 'ic_launcher',
          defType: 'mipmap',
        ),
        enableWifiLock: false,
        shouldRequestBatteryOptimizationsOff: requestPermission,
      ),
    );
    return initialized && await FlutterBackground.enableBackgroundExecution();
  }

  @override
  Future<void> dispose() async {
    _disposed = true;
  }

  @override
  Future<void> callForeground(bool active, {bool video = false}) async {
    final android = _notifications
        .resolvePlatformSpecificImplementation<
          AndroidFlutterLocalNotificationsPlugin
        >();
    if (android == null) {
      if (active) {
        throw StateError('Android call foreground service is unavailable.');
      }
      return;
    }
    if (!active) {
      if (_callStarted) await android.stopForegroundService();
      _callStarted = false;
      return;
    }
    final types = <AndroidServiceForegroundType>{
      AndroidServiceForegroundType.foregroundServiceTypeRemoteMessaging,
      if (await Permission.microphone.isGranted)
        AndroidServiceForegroundType.foregroundServiceTypeMicrophone,
      if (video && await Permission.camera.isGranted)
        AndroidServiceForegroundType.foregroundServiceTypeCamera,
    };
    _callStarted = true;
    await android.startForegroundService(
      id: 9002,
      title: 'Waddle call',
      body: video ? 'Your video call is active' : 'Your voice call is active',
      startType: AndroidServiceStartType.startNotSticky,
      foregroundServiceTypes: types,
      notificationDetails: const AndroidNotificationDetails(
        'waddle-flutter-active-call',
        'Active calls',
        importance: Importance.low,
        priority: Priority.low,
        ongoing: true,
        playSound: false,
        enableVibration: false,
        visibility: NotificationVisibility.private,
      ),
    );
  }
}

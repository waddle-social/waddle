import 'dart:async';
import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_flutter/runtime/context.dart';
import 'package:waddle_flutter/runtime/notification_service.dart';
import 'package:waddle_flutter/transport/transport.dart';

class FakeNotificationBackend implements NotificationBackend {
  void Function(Map<String, dynamic>)? emit;
  final notices = <NotificationNotice>[];
  final unsubscribed = <String>[];
  int clears = 0;
  bool permission = true;
  bool backgroundActive = false;
  bool canBackground = true;
  final backgroundPrompts = <bool>[];
  int backgroundCalls = 0;
  String? lastInstance;
  Map<String, dynamic>? initialAction;
  @override
  bool localDeliveryOnly = false;
  @override
  bool get supported => !localDeliveryOnly;
  @override
  bool get supportsBackground => !localDeliveryOnly;
  @override
  Future<void> initialize(void Function(Map<String, dynamic>) callback) async {
    emit = callback;
    if (initialAction != null) callback(initialAction!);
  }

  @override
  Future<void> account(
    String? owner,
    String? instance, {
    required bool enabled,
    required bool visible,
    bool sound = true,
  }) async {}
  @override
  Future<bool> requestPermission() async => permission;
  @override
  Future<bool> ensurePermission({bool requestOnce = false}) async => permission;
  @override
  Future<List<String>> distributors() async => ['org.example.distributor'];
  @override
  Future<String?> distributor() async => 'org.example.distributor';
  @override
  Future<void> selectDistributor(String value) async {}
  @override
  Future<void> subscribe(String instance, String vapid) async {
    lastInstance = instance;
    emit!({
      'type': 'endpoint',
      'instance': instance,
      'endpoint': {
        'endpoint': 'https://push.example.org/install',
        'p256dh': 'public-key',
        'auth': 'auth-key',
      },
    });
  }

  @override
  Future<void> unsubscribe(String instance) async {
    unsubscribed.add(instance);
  }

  @override
  Future<void> show(NotificationNotice notice) async {
    notices.add(notice);
  }

  @override
  Future<void> clear() async {
    clears++;
  }

  final canceled = <int>[];
  @override
  Future<void> cancel(int id) async {
    canceled.add(id);
  }

  @override
  Future<void> settings() async {}
  @override
  Future<bool> background(
    bool enabled, {
    bool requestPermission = false,
  }) async {
    backgroundCalls++;
    backgroundPrompts.add(requestPermission);
    return backgroundActive = enabled && canBackground;
  }

  @override
  Future<void> dispose() async {}
  final callForegroundChanges = <(bool, bool)>[];
  @override
  Future<void> callForeground(bool active, {bool video = false}) async {
    callForegroundChanges.add((active, video));
  }
}

class FakeFcmBackend extends FakeNotificationBackend
    implements FcmNotifications {
  @override
  bool fcmAvailable = true;
  String token = 'token-one';
  final retired = <String>[];
  late Future<void> Function(NotificationPlatformEvent) receive;
  @override
  Future<void> initializeFcm(
    Future<void> Function(NotificationPlatformEvent) callback,
  ) async {
    receive = callback;
  }

  @override
  Future<Map<String, dynamic>> subscribeFcm(String instance) async {
    lastInstance = instance;
    return {'registration_token': token, 'p256dh': 'key', 'auth': 'secret'};
  }

  @override
  Future<void> retireFcm(String instance) async {
    retired.add(instance);
  }
}

class FakeNotificationContext implements RuntimeContext {
  @override
  String? jid = 'david@example.org';
  @override
  int generation = 1;
  @override
  Map<String, dynamic> session = {
    'state': 'ready',
    'jid': 'david@example.org',
    'xmppLocalpart': 'david',
  };
  @override
  final preferences = <String, dynamic>{
    'notificationsEnabled': true,
    'messageSoundsEnabled': true,
  };
  @override
  final persistent = <String, dynamic>{};
  @override
  RustTransport? get transport => null;
  final calls = <(String, Map<String, dynamic>)>[];
  final emitted = <(String, Map<String, dynamic>)>[];
  Future<dynamic> Function(String, Map<String, dynamic>)? intercept;
  @override
  Future<dynamic> rpc(
    String method, [
    Map<String, dynamic> args = const {},
  ]) async {
    calls.add((method, Map.of(args)));
    if (intercept != null) return intercept!(method, args);
    return switch (method) {
      'fetch_push_vapid_public_key' => {
        'type': 'advertised',
        'public_key': 'vapid-key',
        'kid': 'key-1',
      },
      'register_push_device' => {'node': 'node-1', 'device_id': 'device-1'},
      'enable_push_notifications' || 'disable_push_device' => true,
      _ => [],
    };
  }

  @override
  void emit(String topic, Map<String, dynamic> data) {
    emitted.add((topic, data));
  }

  @override
  Future<void> persist() async {}
  @override
  Future<void> refreshHome() async {}
  @override
  Future<dynamic> platform(
    String method, [
    Map<String, dynamic> args = const {},
  ]) async => null;
}

const payload = {
  'v': 1,
  'class': 'dm',
  'conversation': 'alex@example.org',
  'item': 'archive-item',
  'unread': 2,
};
Future<(NotificationService, FakeNotificationContext, FakeNotificationBackend)>
ready({
  Future<void> Function(Duration)? delay,
  NotificationReply? reply,
  NotificationCallAction? callAction,
}) async {
  final context = FakeNotificationContext();
  final backend = FakeNotificationBackend();
  final service = NotificationService(
    context,
    backend: backend,
    delay: delay ?? (_) async {},
    reply: reply,
    callAction: callAction,
  );
  await service.initialize();
  service.onEvent({'type': 'connected'});
  await service.idle;
  await service.idle;
  return (service, context, backend);
}

void main() {
  test(
    'FCM uses typed device credentials, rotates tokens, fences delivery and can switch to UnifiedPush',
    () async {
      final backend = FakeFcmBackend();
      final context = FakeNotificationContext();
      final service = NotificationService(
        context,
        backend: backend,
        delay: (_) async {},
      );
      addTearDown(service.dispose);
      await service.initialize();
      service.onEvent({'type': 'connected'});
      await service.idle;
      final instance = backend.lastInstance!;
      Map<String, dynamic> registration() =>
          context.calls.lastWhere((c) => c.$1 == 'register_push_device').$2;
      expect(registration()['credentials'], {
        'type': 'fcm',
        'registration_token': 'token-one',
        'p256dh': 'key',
        'auth': 'secret',
      });
      expect(
        context.calls.where((c) => c.$1 == 'fetch_push_vapid_public_key'),
        isEmpty,
      );
      backend.token = 'token-two';
      await backend.receive({'type': 'fcmTokenChanged', 'instance': instance});
      expect(
        runtimeMap(registration()['credentials'])['registration_token'],
        'token-two',
      );
      final push = {
        'type': 'push',
        'transport': 'fcm',
        'instance': instance,
        'decrypted': true,
        'content': utf8.encode(jsonEncode(payload)),
      };
      await backend.receive({...push, 'instance': 'old-recipient'});
      await backend.receive({...push, 'decrypted': false});
      expect(backend.notices, isEmpty);
      await backend.receive(push);
      expect(backend.notices, hasLength(1));
      await service.invoke('push.choose');
      expect(backend.retired, contains(instance));
      expect(
        context.calls.where((c) => c.$1 == 'fetch_push_vapid_public_key'),
        isNotEmpty,
      );
      await backend.receive({
        ...push,
        'content': utf8.encode(jsonEncode({...payload, 'item': 'stale'})),
      });
      expect(backend.notices, hasLength(1));
    },
  );

  test(
    'a notification action from another issuer cannot reply as the same JID',
    () async {
      final replies = <String>[];
      final (service, context, backend) = await ready(
        reply: ({required jid, required group, threadId, required body}) async {
          replies.add(body);
        },
      );
      addTearDown(service.dispose);
      final oldInstance = backend.lastInstance;
      expect(oldInstance, isNotNull);
      context.session['serverOrigin'] = 'https://second.example.org';
      context.generation++;
      service.onAccountChanged();
      service.onEvent({'type': 'connected'});
      await service.idle;
      await service.idle;
      expect(backend.lastInstance, isNot(oldInstance));
      await service.handleOwnerEvent({
        'type': 'action',
        'action': 'reply',
        'input': 'private',
        'payload': {...payload, 'owner': context.jid, 'instance': oldInstance},
      });
      expect(replies, isEmpty);
    },
  );

  test(
    'desktop messages use the active session without registering for push',
    () async {
      final context = FakeNotificationContext();
      final backend = FakeNotificationBackend()..localDeliveryOnly = true;
      final service = NotificationService(context, backend: backend);
      final delivery = <Map<String, dynamic>>[];
      final subscription = service.events.listen((event) {
        if (event['topic'] == 'delivery') {
          delivery.add(Map<String, dynamic>.from(event['data'] as Map));
        }
      });
      addTearDown(subscription.cancel);
      addTearDown(service.dispose);
      await service.initialize();
      service.onEvent({'type': 'connected'});
      await service.idle;
      await service.invoke('app.visibility', {'visible': false});
      service.onEvent({
        'type': 'message',
        'data': {
          'body': 'Hello',
          'from': 'alex@example.org/phone',
          'message_type': 'chat',
          'stanza_ids': [
            {'by': context.jid, 'id': 'a1'},
          ],
        },
      });
      await service.idle;
      expect(backend.notices.single.payload['item'], 'a1');
      expect(delivery.last['localDeliveryOnly'], isTrue);
      expect(delivery.last['pushEnabled'], isFalse);
      expect(delivery.last['status'], contains('while Waddle is running'));
      expect(context.calls.where((call) => call.$1.contains('push')), isEmpty);
    },
  );

  test(
    'validated incoming call notifies without autoaccept and fences actions by SID',
    () async {
      final actions = <String>[];
      final (service, context, backend) = await ready(
        callAction: (action, sid) async {
          actions.add('$action:$sid');
        },
      );
      await service.invoke('push.callState', {
        'generation': context.generation,
        'state': 'incoming',
        'sid': 'ring-A',
        'peerJid': 'alex@example.org',
        'video': true,
      });
      final noticeA = backend.notices.last;
      expect(noticeA.incomingCall, isTrue);
      expect(noticeA.body, 'Incoming video call');
      expect(actions, isEmpty);
      await service.handleOwnerEvent({
        'type': 'action',
        'action': 'open',
        'payload': noticeA.payload,
      });
      expect(
        actions,
        isEmpty,
        reason: 'Opening a full-screen intent never accepts a caller',
      );
      await service.invoke('push.callState', {
        'generation': context.generation,
        'state': 'incoming',
        'sid': 'ring-B',
        'peerJid': 'blair@example.org',
      });
      final noticeB = backend.notices.last;
      await service.handleOwnerEvent({
        'type': 'action',
        'action': 'answer',
        'payload': noticeA.payload,
      });
      await service.handleOwnerEvent({
        'type': 'action',
        'action': 'answer',
        'payload': {...noticeB.payload, 'owner': 'other@example.org'},
      });
      expect(actions, isEmpty);
      await service.handleOwnerEvent({
        'type': 'action',
        'action': 'decline',
        'payload': noticeB.payload,
      });
      expect(actions, ['decline:ring-B']);
      await service.invoke('push.callState', {
        'generation': context.generation,
        'state': 'ended',
        'sid': 'ring-B',
      });
      await service.handleOwnerEvent({
        'type': 'action',
        'action': 'answer',
        'payload': noticeB.payload,
      });
      expect(actions, ['decline:ring-B']);
      expect(backend.canceled, contains(9003));
      context.jid = null;
      context.session = {'state': 'signedOut'};
      context.generation++;
      service.onAccountChanged();
      await service.handleOwnerEvent({
        'type': 'action',
        'action': 'answer',
        'payload': noticeB.payload,
      });
      expect(actions, ['decline:ring-B']);
      await service.dispose();
    },
  );

  test(
    'cold-start Answer waits for the same validated incoming call',
    () async {
      final (old, _, oldBackend) = await ready();
      final oldInstance = oldBackend.lastInstance;
      await old.dispose();
      final context = FakeNotificationContext()
        ..jid = null
        ..session = {'state': 'loading'};
      final backend = FakeNotificationBackend()
        ..initialAction = {
          'type': 'action',
          'action': 'answer',
          'payload': {
            'kind': 'call',
            'owner': 'david@example.org',
            'instance': oldInstance,
            'sid': 'same-ring',
          },
        };
      final actions = <String>[];
      final service = NotificationService(
        context,
        backend: backend,
        callAction: (action, sid) async {
          actions.add('$action:$sid');
        },
      );
      await service.initialize();
      context.jid = 'david@example.org';
      context.session = {'state': 'ready', 'jid': context.jid};
      context.generation++;
      service.onAccountChanged();
      await service.idle;
      expect(actions, isEmpty);
      await service.invoke('push.callState', {
        'generation': context.generation,
        'state': 'incoming',
        'sid': 'same-ring',
        'peerJid': 'alex@example.org',
      });
      expect(actions, ['answer:same-ring']);
      await service.dispose();
    },
  );

  test('stale call foreground stop cannot stop a successor service', () async {
    final (service, _, backend) = await ready();
    await service.invoke('push.callForeground', {
      'active': true,
      'video': true,
      'sid': '1:1:A',
    });
    await service.invoke('push.callForeground', {
      'active': true,
      'sid': '1:2:B',
    });
    await service.invoke('push.callForeground', {
      'active': false,
      'sid': '1:1:A',
    });
    expect(backend.callForegroundChanges, [(true, true), (true, false)]);
    await service.invoke('push.callForeground', {
      'active': false,
      'sid': '1:2:B',
    });
    expect(backend.callForegroundChanges.last.$1, isFalse);
    await service.dispose();
  });

  test(
    'denied notification permission never claims effective push delivery',
    () async {
      final context = FakeNotificationContext();
      final backend = FakeNotificationBackend()..permission = false;
      final service = NotificationService(context, backend: backend);
      final snapshots = <Map<String, dynamic>>[];
      final sub = service.events.listen((event) {
        if (event['topic'] == 'delivery') {
          snapshots.add(runtimeMap(event['data']));
        }
      });
      await service.initialize();
      service.onEvent({'type': 'connected'});
      await service.idle;
      await service.idle;
      await Future<void>.delayed(Duration.zero);
      expect(runtimeMap(context.persistent['push'])['enabled'], isTrue);
      expect(snapshots.last['pushEnabled'], isFalse);
      expect(snapshots.last['status'], contains('blocked'));
      await sub.cancel();
      await service.dispose();
    },
  );

  test(
    'delivery resolves push readiness and reports actual background capability',
    () async {
      final (service, context, backend) = await ready();
      final deliveries = <Map<String, dynamic>>[];
      final sub = service.events.listen((event) {
        if (event['topic'] == 'delivery') {
          deliveries.add(runtimeMap(event['data']));
        }
      });
      await service.invoke('push.reconcile');
      await service.idle;
      await Future<void>.delayed(Duration.zero);
      expect(deliveries.last['resolved'], isTrue);
      expect(deliveries.last['pushEnabled'], isTrue);
      expect(
        deliveries.last['stayConnected'],
        isFalse,
        reason: 'Default fallback FGS stops once push is available',
      );
      context.preferences['stayConnected'] = true;
      backend.canBackground = false;
      await service.invoke('push.reconcile');
      await service.idle;
      await Future<void>.delayed(Duration.zero);
      expect(
        deliveries.last['stayConnected'],
        isFalse,
        reason:
            'A saved preference is not evidence that Android allowed an FGS',
      );
      expect(
        backend.backgroundPrompts,
        everyElement(isFalse),
        reason: 'A hidden runtime never requests battery permission',
      );
      await sub.cancel();
      await service.dispose();
    },
  );

  test(
    'a queued inline reply is not retried and releases temporary background support',
    () async {
      var attempts = 0;
      final (service, context, backend) = await ready(
        reply: ({required jid, required group, required body, threadId}) async {
          attempts++;
          throw StateError('Your reply is queued for the next connection.');
        },
      );
      final owned = {
        ...payload,
        'owner': context.jid,
        'instance': backend.lastInstance,
      };
      await service.handleOwnerEvent({
        'type': 'action',
        'action': 'reply',
        'payload': owned,
        'input': 'Keep this reply',
      });
      expect(attempts, 1);
      expect(backend.notices.single.body, 'Open Waddle to check your reply');
      expect(backend.backgroundActive, isFalse);
      expect(backend.backgroundPrompts, everyElement(isFalse));
      await service.dispose();
    },
  );

  test('push dedupe survives an account runtime restart', () async {
    final (first, context, backend) = await ready();
    await first.handleOwnerEvent({
      'type': 'push',
      'decrypted': true,
      'instance': backend.lastInstance,
      'content': payload,
    });
    expect(backend.notices, hasLength(1));
    expect(context.persistent['pushAnnouncedItems'], contains('archive-item'));
    await first.dispose();
    final nextBackend = FakeNotificationBackend();
    final next = NotificationService(
      context,
      backend: nextBackend,
      delay: (_) async {},
    );
    await next.initialize();
    next.onEvent({'type': 'connected'});
    await next.idle;
    await next.idle;
    await next.handleOwnerEvent({
      'type': 'push',
      'decrypted': true,
      'instance': nextBackend.lastInstance,
      'content': payload,
    });
    expect(nextBackend.notices, isEmpty);
    await next.dispose();
  });

  test(
    'disposing an uninitialized losing-owner service cannot stop its peer',
    () async {
      final backend = FakeNotificationBackend()..backgroundActive = true;
      final service = NotificationService(
        FakeNotificationContext(),
        backend: backend,
      );
      await service.dispose();
      expect(backend.backgroundCalls, 0);
      expect(backend.backgroundActive, isTrue);
    },
  );

  test(
    'cold-start notification action survives plugin initialization and secure restore',
    () async {
      final (previous, _, priorBackend) = await ready();
      final oldInstance = priorBackend.lastInstance;
      await previous.dispose();
      final context = FakeNotificationContext()
        ..jid = null
        ..session = {'state': 'loading'};
      final backend = FakeNotificationBackend()
        ..initialAction = {
          'type': 'action',
          'action': 'open',
          'payload': {
            ...payload,
            'owner': 'david@example.org',
            'instance': oldInstance,
          },
        };
      final service = NotificationService(
        context,
        backend: backend,
        delay: (_) async {},
      );
      final events = <Map<String, dynamic>>[];
      final subscription = service.events.listen(events.add);
      await service.initialize();
      await service.idle;
      context.jid = 'david@example.org';
      context.session = {'state': 'ready', 'jid': context.jid};
      context.generation++;
      service.onAccountChanged();
      service.onEvent({'type': 'connected'});
      await service.idle;
      await service.idle;
      await Future<void>.delayed(Duration.zero);
      expect(
        events.where((event) => event['topic'] == 'navigate'),
        hasLength(1),
      );
      await subscription.cancel();
      await service.dispose();
    },
  );

  test(
    'v1 envelope rejects malformed schemas, classes, resources and identifiers',
    () {
      expect(
        NotificationEnvelope.parse(utf8.encode('${jsonEncode(payload)}\u0000')),
        isNotNull,
      );
      for (final bad in [
        {...payload, 'v': 2},
        {...payload, 'class': 'unknown'},
        {...payload, 'conversation': 'alex@example.org/resource'},
        {...payload, 'conversation': 'a@@example.org'},
        {...payload, 'conversation': 'a\n@example.org'},
        {...payload, 'item': ''},
        {...payload, 'thread': 3},
      ]) {
        expect(NotificationEnvelope.parse(bad), isNull, reason: '$bad');
      }
    },
  );

  test(
    'registration is Web Push for this app and persists per-device identity',
    () async {
      final (service, context, _) = await ready();
      final registration = context.calls
          .firstWhere((call) => call.$1 == 'register_push_device')
          .$2;
      expect(registration['app_id'], 'social.waddle.flutter');
      expect(registration['credentials'], containsPair('type', 'web_push'));
      expect(
        runtimeMap(
          runtimeMap(context.persistent['push'])['device'],
        )['device_id'],
        'device-1',
      );
      expect(
        context.calls.where((call) => call.$1 == 'enable_push_notifications'),
        isNotEmpty,
      );
      await service.dispose();
    },
  );

  test(
    'only decrypted current-install pushes notify, without message content',
    () async {
      final (service, context, backend) = await ready();
      // The instance appears in native/backend subscription requests, not server credentials.
      String? current;
      final original = backend.emit;
      backend.emit = (event) {
        current = event['instance'] as String?;
        original!(event);
      };
      await service.invoke('push.reconcile');
      await service.idle;
      final push = {
        'type': 'push',
        'instance': current,
        'content': payload,
        'decrypted': true,
      };
      await service.handleOwnerEvent({...push, 'decrypted': false});
      await service.handleOwnerEvent({...push, 'instance': 'old-install'});
      expect(backend.notices, isEmpty);
      await service.handleOwnerEvent(push);
      await service.handleOwnerEvent(push);
      expect(backend.notices, hasLength(1));
      expect(backend.notices.single.body, '2 new messages');
      expect(backend.notices.single.payload['owner'], context.jid);
      await service.dispose();
    },
  );

  test('logout during live grace cannot resurrect a private banner', () async {
    final grace = Completer<void>();
    final (service, context, backend) = await ready(delay: (_) => grace.future);
    String? current;
    final original = backend.emit;
    backend.emit = (event) {
      current = event['instance'] as String?;
      original!(event);
    };
    await service.invoke('push.reconcile');
    await service.idle;
    final handling = service.handleOwnerEvent({
      'type': 'push',
      'instance': current,
      'content': payload,
      'decrypted': true,
    });
    await Future<void>.delayed(Duration.zero);
    context.generation++;
    context.jid = null;
    context.session = {'state': 'signedOut'};
    service.onAccountChanged();
    grace.complete();
    await handling;
    expect(backend.notices, isEmpty);
    await service.dispose();
  });

  test(
    'opt-out disables only the registered device and clears local notifications',
    () async {
      final (service, context, backend) = await ready();
      context.preferences['notificationsEnabled'] = false;
      await service.invoke('push.reconcile');
      final disable = context.calls
          .where((call) => call.$1 == 'disable_push_device')
          .single
          .$2;
      expect(disable, {
        'push_service_jid': 'push.example.org',
        'node': 'node-1',
        'device_id': 'device-1',
      });
      expect(
        context.calls.where((call) => call.$1 == 'disable_push_notifications'),
        isEmpty,
      );
      expect(backend.unsubscribed, isNotEmpty);
      await service.dispose();
    },
  );

  test(
    'desktop notification actions mark the owned thread read or open its composer',
    () async {
      final (service, context, backend) = await ready();
      await service.invoke('push.reconcile');
      await service.idle;
      final owned = {
        ...payload,
        'owner': context.jid,
        'instance': backend.lastInstance,
        'thread': 'thread-1',
      };
      Future<void> action(String id, Map<String, dynamic> target) =>
          service.handleOwnerEvent({
            'type': 'action',
            'action': id,
            'payload': target,
          });
      final previouslyCanceled = backend.canceled.length;
      await action('markRead', {...owned, 'instance': 'retired'});
      expect(
        context.calls.where((call) => call.$1 == 'mark_inbox_read'),
        isEmpty,
      );
      await action('markRead', owned);
      expect(
        context.calls.where((call) => call.$1 == 'mark_inbox_read').single.$2,
        {'partner_jid': 'alex@example.org', 'thread_id': 'thread-1'},
      );
      expect(backend.canceled, hasLength(previouslyCanceled + 1));
      final navigation = service.events.firstWhere(
        (event) => event['topic'] == 'navigate',
      );
      await action('compose', owned);
      expect((await navigation)['data'], containsPair('compose', true));
      await service.dispose();
    },
  );

  test('tap and inline reply require matching account and instance', () async {
    final replies = <String>[];
    final (service, context, backend) = await ready(
      reply: ({required jid, required group, required body, threadId}) async {
        replies.add('$jid:$group:$threadId:$body');
      },
    );
    String? current;
    final original = backend.emit;
    backend.emit = (event) {
      current = event['instance'] as String?;
      original!(event);
    };
    await service.invoke('push.reconcile');
    await service.idle;
    final owned = {...payload, 'owner': context.jid, 'instance': current};
    await service.handleOwnerEvent({
      'type': 'action',
      'action': 'reply',
      'payload': {...owned, 'owner': 'other@example.org'},
      'input': 'Do not send',
    });
    expect(replies, isEmpty);
    await service.handleOwnerEvent({
      'type': 'action',
      'action': 'reply',
      'payload': {...owned, 'thread': 'thread-1'},
      'input': 'Hello',
    });
    expect(replies, ['alex@example.org:false:thread-1:Hello']);
    final emitted = <Map<String, dynamic>>[];
    final sub = service.events.listen(emitted.add);
    await service.handleOwnerEvent({
      'type': 'action',
      'action': 'open',
      'payload': {...owned, 'thread': 'thread-1'},
    });
    await Future<void>.delayed(Duration.zero);
    expect(emitted.single['topic'], 'navigate');
    expect(runtimeMap(emitted.single['data'])['threadId'], 'thread-1');
    await sub.cancel();
    await service.dispose();
  });

  test(
    'live notifications honor mute/mention policy and do not announce corrections',
    () async {
      final (service, context, backend) = await ready();
      final message = {
        'body': 'Not exposed in banner',
        'from': 'alex@example.org/resource',
        'message_type': 'chat',
        'stanza_ids': [
          {'by': context.jid, 'id': 'live-1'},
        ],
      };
      context.persistent['notificationModes'] = {'alex@example.org': 'never'};
      service.onEvent({'type': 'message', 'data': message});
      await service.idle;
      expect(backend.notices, isEmpty);
      context.persistent['notificationModes'] = {'alex@example.org': 'always'};
      service.onEvent({
        'type': 'message',
        'data': {...message, 'replaces_id': 'old-message'},
      });
      await service.idle;
      expect(backend.notices, isEmpty);
      service.onEvent({'type': 'message', 'data': message});
      await service.idle;
      expect(backend.notices.single.body, 'New message');
      expect(backend.notices.single.body, isNot(contains('Not exposed')));
      await service.dispose();
    },
  );
}

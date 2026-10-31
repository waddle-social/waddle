import 'dart:async';

import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_flutter/runtime/context.dart';
import 'package:waddle_flutter/runtime/session_auth.dart';
import 'package:waddle_flutter/runtime/session_runtime.dart';
import 'package:waddle_flutter/runtime/storage.dart';
import 'package:waddle_flutter/transport/transport.dart';

class _Storage implements RuntimeStorage {
  final values = <String, String>{};
  @override
  Future<String?> read(String key) async => values[key];
  @override
  Future<void> write(String key, String? value) async {
    if (value == null) {
      values.remove(key);
    } else {
      values[key] = value;
    }
  }
}

class _DelayedStorage extends _Storage {
  Completer<void>? accountWrite;
  @override
  Future<void> write(String key, String? value) async {
    if (key.startsWith('account:') && accountWrite != null) {
      await accountWrite!.future;
    }
    await super.write(key, value);
  }
}

Map<String, dynamic> _credentials(String id, String owner) => {
  'session_id': id,
  'jid': owner,
  'username': owner.split('@').first,
  'xmpp_localpart': owner.split('@').first,
  'is_expired': false,
  'xmpp_websocket_url': 'wss://xmpp.example.org/xmpp-websocket',
};

class _Auth implements AuthGateway {
  Future<Map<String, dynamic>?> Function(String)? readSession;
  Future<Map<String, dynamic>> Function(String)? poll;
  int sessionReads = 0;
  final revoked = <String>[];
  @override
  Future<List<Map<String, dynamic>>> providers() async => [
    {'id': 'community', 'display_name': 'Community'},
  ];
  @override
  Future<Map<String, dynamic>> startDevice(String provider) async => {
    'device_code': 'private-device-code',
    'user_code': 'ABCD-EFGH',
    'verification_uri': 'https://example.org/approve',
    'interval': 1,
  };
  @override
  Future<Map<String, dynamic>> pollDevice(String code) async =>
      poll != null ? poll!(code) : {'status': 'pending'};
  @override
  Future<Map<String, dynamic>?> session(String id) async {
    sessionReads++;
    return readSession != null
        ? readSession!(id)
        : _credentials(id, 'me@example.org');
  }

  @override
  Future<void> logout(String id) async {
    revoked.add(id);
  }
}

class _Transport implements RustTransport {
  final controller = StreamController<Map<String, dynamic>>.broadcast();
  final calls = <(String, Map<String, dynamic>)>[];
  final localRows = <Map<String, dynamic>>[];
  Future<dynamic> Function(String, Map<String, dynamic>)? handler;
  bool closed = false;
  int closeCalls = 0;
  Future<void> Function()? closeHandler;
  @override
  Stream<Map<String, dynamic>> get events => controller.stream;
  @override
  Future<dynamic> rpc(
    String method, [
    Map<String, dynamic> args = const {},
  ]) async {
    calls.add((method, Map.of(args)));
    if (handler != null) {
      return handler!(method, args);
    }
    if (method == 'timeline_local_echo') {
      localRows.add({
        'id': args['origin_id'],
        'origin_id': args['origin_id'],
        'own': true,
      });
    }
    if (method.startsWith('timeline_')) {
      return {'items': localRows};
    }
    return switch (method) {
      'discover_topology' => {'channels': <dynamic>[]},
      'fetch_inbox' => {'conversations': <dynamic>[]},
      'is_community_owner' => false,
      'send_chat_message' || 'send_groupchat_message' => {
        'type': 'sent',
        'stanza_id': runtimeMap(args['options'])['stanza_id'],
      },
      'fetch_user_bookmarks' || 'fetch_dm_bookmarks' => <dynamic>[],
      _ => null,
    };
  }

  @override
  Future<void> close() async {
    closeCalls++;
    await closeHandler?.call();
    closed = true;
  }
}

Future<void> _flush() async {
  for (var i = 0; i < 8; i++) {
    await Future<void>.delayed(Duration.zero);
  }
}

void main() {
  test(
    'hidden push delivery pauses the socket and foreground resumes a fresh handle',
    () async {
      final storage = _Storage()..values['session'] = 'private-token';
      final handles = <_Transport>[];
      final runtime = SessionRuntime(
        storage: storage,
        auth: _Auth(),
        initiallyVisible: false,
        transportFactory: () async {
          final handle = _Transport();
          handles.add(handle);
          return handle;
        },
        platformInvoker: (_, _) async => null,
      );
      addTearDown(runtime.dispose);
      await runtime.start();
      await _flush();
      expect(
        handles,
        hasLength(1),
      ); // Unresolved delivery conservatively connects.
      await runtime.conversations.notificationReply(
        jid: 'friend@example.org',
        group: false,
        body: 'Retained offline reply',
      );
      await runtime.effectiveBackgroundDemand(false, pushEnabled: true);
      expect(handles.single.closed, true);
      expect(runtime.transport, isNull);
      expect(runtime.session['state'], 'ready');
      expect(storage.values['session'], 'private-token');
      expect(
        storage.values['account:me@example.org'],
        contains('Retained offline reply'),
      );
      expect(
        runtimeRows(runtime.persistent['conversationOutbox']),
        hasLength(1),
      );
      await runtime.onVisibility(true);
      expect(handles, hasLength(2));
      expect(handles.last.closed, false);
      expect(
        handles.last.calls
            .singleWhere((call) => call.$1 == 'connect')
            .$2['resumable'],
        false,
      );
    },
  );

  test(
    'background reply is persisted before ready and keeps its lease until ACK',
    () async {
      final storage = _Storage()..values['session'] = 'private-token';
      final handles = <_Transport>[];
      final runtime = SessionRuntime(
        storage: storage,
        auth: _Auth(),
        initiallyVisible: false,
        transportFactory: () async {
          final handle = _Transport();
          handles.add(handle);
          return handle;
        },
        platformInvoker: (_, _) async => null,
      );
      addTearDown(runtime.dispose);
      await runtime.start();
      await _flush();
      await runtime.effectiveBackgroundDemand(false, pushEnabled: true);
      var completed = false;
      final reply = runtime
          .withConnectionDemand(
            () => runtime.conversations.notificationReply(
              jid: 'friend@example.org',
              group: false,
              body: 'Persist me before connecting',
              waitForConnection: true,
            ),
            waitForConnection: false,
          )
          .then((_) => completed = true);
      await _flush();
      expect(handles, hasLength(2));
      final active = handles.last;
      final row = runtimeRows(runtime.persistent['conversationOutbox']).single;
      expect(
        storage.values['account:me@example.org'],
        contains('Persist me before connecting'),
      );
      expect(
        active.calls.where((call) => call.$1 == 'send_chat_message'),
        isEmpty,
      );
      expect(completed, false);
      active.controller.add({'type': 'connected', 'data': null});
      await _flush();
      expect(
        active.calls.where((call) => call.$1 == 'send_chat_message'),
        hasLength(1),
      );
      expect(completed, false);
      expect(active.closed, false);
      active.controller.add({
        'type': 'deliveryAcked',
        'data': row['origin_id'],
      });
      await reply;
      expect(completed, true);
      expect(active.closed, true);
      expect(runtime.transport, isNull);
      expect(runtimeRows(runtime.persistent['conversationOutbox']), isEmpty);
    },
  );

  test(
    'live call and pending signaling retain a hidden push connection',
    () async {
      final storage = _Storage()..values['session'] = 'private-token';
      final transport = _Transport();
      final platform = StreamController<Map<String, dynamic>>.broadcast();
      final runtime = SessionRuntime(
        storage: storage,
        auth: _Auth(),
        initiallyVisible: false,
        transportFactory: () async => transport,
        platformInvoker: (_, _) async => null,
      );
      runtime.attachPlatformEvents(platform.stream);
      addTearDown(platform.close);
      addTearDown(runtime.dispose);
      await runtime.start();
      await _flush();
      platform.add({
        'topic': 'call',
        'generation': runtime.generation,
        'data': {'state': 'active'},
      });
      await _flush();
      await runtime.effectiveBackgroundDemand(false, pushEnabled: true);
      expect(transport.closed, false);
      platform.add({
        'topic': 'call',
        'generation': runtime.generation,
        'data': {'state': 'ended', 'signaling': true},
      });
      await _flush();
      expect(transport.closed, false);
      platform.add({
        'topic': 'call',
        'generation': runtime.generation,
        'data': {'state': 'ended', 'signaling': false},
      });
      await _flush();
      expect(transport.closed, true);
    },
  );

  test(
    'failed retirement fences replacements until the old close succeeds',
    () async {
      final storage = _Storage()..values['session'] = 'private-token';
      var failClose = true;
      final first = _Transport()
        ..closeHandler = () async {
          if (failClose) {
            throw StateError('socket is not quiescent');
          }
        };
      var factories = 0;
      final runtime = SessionRuntime(
        storage: storage,
        auth: _Auth(),
        transportFactory: () async => factories++ == 0 ? first : _Transport(),
        platformInvoker: (_, _) async => null,
      );
      await runtime.start();
      await _flush();
      await runtime.onVisibility(false);
      await expectLater(
        runtime.effectiveBackgroundDemand(false, pushEnabled: true),
        throwsStateError,
      );
      await expectLater(runtime.onVisibility(true), throwsStateError);
      expect(factories, 1);
      expect(first.closed, false);
      failClose = false;
      await runtime.invoke('session.retry', {});
      expect(first.closed, true);
      expect(factories, 2);
      await runtime.dispose();
    },
  );

  test(
    'failed disposal is never acknowledged and can retry the same old handle',
    () async {
      final storage = _Storage()..values['session'] = 'private-token';
      var failClose = true;
      final transport = _Transport()
        ..closeHandler = () async {
          if (failClose) {
            throw StateError('socket is not quiescent');
          }
        };
      final runtime = SessionRuntime(
        storage: storage,
        auth: _Auth(),
        transportFactory: () async => transport,
        platformInvoker: (_, _) async => null,
      );
      await runtime.start();
      await _flush();
      await expectLater(runtime.dispose(), throwsStateError);
      await expectLater(runtime.dispose(), throwsStateError);
      expect(transport.closeCalls, 2);
      failClose = false;
      await runtime.dispose();
      expect(transport.closed, true);
      expect(transport.closeCalls, 3);
    },
  );

  test(
    'handoff waits for a pending factory and its disconnect fence',
    () async {
      final storage = _Storage()..values['session'] = 'private-token';
      final factory = Completer<RustTransport>();
      final closed = Completer<void>();
      final transport = _Transport()..closeHandler = () => closed.future;
      final runtime = SessionRuntime(
        storage: storage,
        auth: _Auth(),
        transportFactory: () => factory.future,
        platformInvoker: (_, _) async => null,
      );
      await runtime.start();
      await _flush();
      var released = false;
      final disposed = runtime.dispose().then((_) => released = true);
      await _flush();
      expect(released, false);
      factory.complete(transport);
      await _flush();
      expect(released, false);
      expect(transport.calls.where((call) => call.$1 == 'connect'), isEmpty);
      closed.complete();
      await disposed;
      expect(released, true);
      expect(transport.closed, true);
    },
  );

  test(
    'native resync overflow replaces the quarantined transport immediately',
    () async {
      final storage = _Storage()..values['session'] = 'private-token';
      final handles = <_Transport>[];
      final runtime = SessionRuntime(
        storage: storage,
        auth: _Auth(),
        reconnectDelay: (_) => Duration.zero,
        transportFactory: () async {
          final handle = _Transport();
          handles.add(handle);
          return handle;
        },
        platformInvoker: (_, _) async => null,
      );
      addTearDown(runtime.dispose);
      await runtime.start();
      await _flush();
      handles.single.controller.add({
        'type': 'error',
        'data': {'code': 'resync_required', 'message': 'queue overflow'},
      });
      await _flush();
      expect(handles, hasLength(2));
      expect(handles.first.closed, true);
    },
  );

  test('logout stops call signaling before clearing its identity', () async {
    final storage = _Storage()..values['session'] = 'private-token';
    final transport = _Transport();
    final platform = StreamController<Map<String, dynamic>>.broadcast();
    final operations = <String>[];
    late final SessionRuntime runtime;
    runtime = SessionRuntime(
      storage: storage,
      auth: _Auth(),
      transportFactory: () async => transport,
      platformInvoker: (method, args) async {
        operations.add(method);
        expect(runtime.jid, 'me@example.org');
        expect(runtime.session['fullJid'], isNotNull);
        expect(args['generation'], runtime.generation);
      },
    );
    addTearDown(platform.close);
    addTearDown(runtime.dispose);
    runtime.attachPlatformEvents(platform.stream);
    await runtime.start();
    await _flush();
    platform.add({
      'topic': 'call',
      'generation': runtime.generation,
      'data': {'state': 'active'},
    });
    await _flush();
    await runtime.invoke('session.logout', {});
    expect(operations, ['call.hangup']);
    expect(runtime.session['state'], 'signedOut');
    expect(transport.closed, true);
    expect(storage.values['session'], isNull);
  });

  test(
    'logout clears saved credentials even if socket retirement fails',
    () async {
      final storage = _Storage()..values['session'] = 'private-token';
      final auth = _Auth();
      var failClose = true;
      final transport = _Transport()
        ..closeHandler = () async {
          if (failClose) {
            throw StateError('socket is not quiescent');
          }
        };
      final runtime = SessionRuntime(
        storage: storage,
        auth: auth,
        transportFactory: () async => transport,
        platformInvoker: (_, _) async => null,
      );
      await runtime.start();
      await _flush();
      await expectLater(runtime.invoke('session.logout', {}), throwsStateError);
      expect(runtime.session['state'], 'signedOut');
      expect(storage.values['session'], isNull);
      expect(auth.revoked, ['private-token']);
      expect(transport.closed, false);
      failClose = false;
      await runtime.dispose();
      expect(transport.closed, true);
    },
  );
  test(
    'runtime handoff flushes queued account data before disconnect acknowledgement',
    () async {
      final storage = _DelayedStorage()..values['session'] = 'private-token';
      final transport = _Transport();
      final runtime = SessionRuntime(
        storage: storage,
        auth: _Auth(),
        transportFactory: () async => transport,
        platformInvoker: (_, _) async => null,
      );
      await runtime.start();
      await _flush();
      storage.accountWrite = Completer<void>();
      runtime.persistent['conversationOutbox'] = [
        {'body': 'Retained draft'},
      ];
      final written = runtime.persist();
      await _flush();
      var released = false;
      final disposed = runtime.dispose().then((_) => released = true);
      await _flush();
      expect(released, false);
      expect(transport.closed, false);
      storage.accountWrite!.complete();
      await written;
      await disposed;
      expect(
        storage.values['account:me@example.org'],
        contains('Retained draft'),
      );
      expect(transport.closed, true);
      expect(released, true);
    },
  );

  test(
    'parallel room opens share one join and preserve its durable membership',
    () async {
      final storage = _Storage()..values['session'] = 'private-token';
      final joined = Completer<dynamic>();
      final transport = _Transport()
        ..handler = (method, args) async =>
            method == 'join_room' ? joined.future : null;
      final runtime = SessionRuntime(
        storage: storage,
        auth: _Auth(),
        transportFactory: () async => transport,
        platformInvoker: (_, _) async => null,
      );
      addTearDown(runtime.dispose);
      await runtime.start();
      await _flush();
      final first = runtime.rpc('join_room', {
        'room_jid': 'room@rooms.example.org',
        'nick': 'me',
      });
      final second = runtime.rpc('join_room', {
        'room_jid': 'room@rooms.example.org',
        'nick': 'me',
      });
      await _flush();
      expect(transport.calls.where((call) => call.$1 == 'join_room').length, 1);
      joined.complete(null);
      await Future.wait([first, second]);
      await runtime.rpc('join_room', {
        'room_jid': 'room@rooms.example.org',
        'nick': 'me',
      });
      expect(transport.calls.where((call) => call.$1 == 'join_room').length, 1);
      expect(runtime.persistent['joinedRooms'], ['room@rooms.example.org']);
    },
  );

  test(
    'failed REST validation keeps saved credentials and offers retry',
    () async {
      final storage = _Storage()..values['session'] = 'private-token';
      final auth = _Auth()
        ..readSession = (_) async => throw const AuthFailure(transient: true);
      var factories = 0;
      final runtime = SessionRuntime(
        storage: storage,
        auth: auth,
        transportFactory: () async {
          factories++;
          return _Transport();
        },
        platformInvoker: (_, _) async => null,
      );
      addTearDown(runtime.dispose);
      final events = <Map<String, dynamic>>[];
      final subscription = runtime.events.listen(events.add);
      addTearDown(subscription.cancel);
      await runtime.start();
      await _flush();
      expect(runtime.session['state'], 'error');
      expect(storage.values['session'], 'private-token');
      expect(factories, 0);
      expect(events.toString(), isNot(contains('private-token')));
    },
  );

  test('cancelled device flow ignores a late completed poll', () async {
    final poll = Completer<Map<String, dynamic>>();
    final auth = _Auth()..poll = (_) => poll.future;
    final storage = _Storage();
    var factories = 0;
    final runtime = SessionRuntime(
      storage: storage,
      auth: auth,
      transportFactory: () async {
        factories++;
        return _Transport();
      },
      platformInvoker: (_, _) async => null,
      delay: (_) async {},
    );
    addTearDown(runtime.dispose);
    await runtime.start();
    await runtime.invoke('login.select', {'id': 'community'});
    await _flush();
    await runtime.invoke('login.cancel', {});
    poll.complete({'status': 'complete', 'session_id': 'late-token'});
    await _flush();
    expect(auth.sessionReads, 0);
    expect(runtime.session['state'], 'signedOut');
    expect(storage.values['session'], isNull);
    expect(factories, 0);
  });

  test(
    'restored account connects with its private token and resumption disabled',
    () async {
      final storage = _Storage()..values['session'] = 'private-token';
      final transport = _Transport();
      final runtime = SessionRuntime(
        storage: storage,
        auth: _Auth(),
        transportFactory: () async => transport,
        platformInvoker: (_, _) async => null,
      );
      addTearDown(runtime.dispose);
      final events = <Map<String, dynamic>>[];
      final subscription = runtime.events.listen(events.add);
      addTearDown(subscription.cancel);
      await runtime.start();
      await _flush();
      final connect = transport.calls
          .singleWhere((call) => call.$1 == 'connect')
          .$2;
      expect(connect['access_token'], 'private-token');
      expect(connect['resumable'], false);
      expect(
        runtime.session['fullJid'],
        'me@example.org/${connect['resource']}',
      );
      expect(events.toString(), isNot(contains('private-token')));
      expect(events.toString(), isNot(contains('private-device-code')));
    },
  );

  test(
    'logout fences an unfinished profile read before its follow-up RPCs',
    () async {
      final storage = _Storage()..values['session'] = 'private-token';
      final card = Completer<dynamic>();
      final transport = _Transport()
        ..handler = (method, args) async =>
            method == 'fetch_vcard4' ? card.future : null;
      final runtime = SessionRuntime(
        storage: storage,
        auth: _Auth(),
        transportFactory: () async => transport,
        platformInvoker: (_, _) async => null,
      );
      addTearDown(runtime.dispose);
      await runtime.start();
      await _flush();
      final pending = runtime.invoke('profile.load', {});
      final assertion = expectLater(pending, throwsA(isA<AccountChanged>()));
      await _flush();
      await runtime.invoke('session.logout', {});
      card.complete({'full_name': 'Old account private name'});
      await assertion;
      expect(
        transport.calls.where((call) => call.$1 == 'fetch_user_pep_profile'),
        isEmpty,
      );
      expect(storage.values['session'], isNull);
      expect(runtime.session['state'], 'signedOut');
    },
  );

  test(
    'invalid persisted session signs out and retains user preferences',
    () async {
      final storage = _Storage()
        ..values.addAll({
          'session': 'expired',
          'preferences': '{"theme":"dark"}',
        });
      final auth = _Auth()..readSession = (_) async => null;
      final runtime = SessionRuntime(
        storage: storage,
        auth: auth,
        transportFactory: () async => _Transport(),
        platformInvoker: (_, _) async => null,
      );
      addTearDown(runtime.dispose);
      await runtime.start();
      expect(runtime.session['state'], 'signedOut');
      expect(runtime.preferences['theme'], 'dark');
      expect(storage.values['session'], isNull);
      expect(storage.values['preferences'], isNotNull);
    },
  );
}

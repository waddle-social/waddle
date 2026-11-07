import 'dart:async';

import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_flutter/runtime/session_auth.dart';
import 'package:waddle_flutter/runtime/session_runtime.dart';
import 'package:waddle_flutter/runtime/storage.dart';
import 'package:waddle_flutter/transport/transport.dart';

class _Storage implements RuntimeStorage {
  final values = <String, String>{'session': 'test-token'};
  Completer<void>? accountWrite;

  @override
  Future<String?> read(String key) async => values[key];

  @override
  Future<void> write(String key, String? value) async {
    if (key.startsWith('account:')) await accountWrite?.future;
    if (value == null) {
      values.remove(key);
    } else {
      values[key] = value;
    }
  }
}

class _Auth implements AuthGateway {
  @override
  Future<Map<String, dynamic>?> session(String id) async => {
    'session_id': id,
    'jid': 'me@example.org',
    'xmpp_websocket_url': 'wss://example.org/xmpp',
  };
  @override
  Future<void> logout(String id) async {}
  @override
  Future<List<Map<String, dynamic>>> providers() async => [];
  @override
  Future<Map<String, dynamic>> startDevice(String provider) async => {};
  @override
  Future<Map<String, dynamic>> pollDevice(String deviceCode) async => {};
}

class _Transport implements RustTransport {
  final controller = StreamController<Map<String, dynamic>>.broadcast(
    sync: true,
  );
  final calls = <String>[];
  Completer<dynamic>? connect, presence, ping;
  bool closed = false;

  @override
  Stream<Map<String, dynamic>> get events => controller.stream;

  @override
  Future<dynamic> rpc(
    String method, [
    Map<String, dynamic> args = const {},
  ]) async {
    calls.add(method);
    if (method == 'connect' && connect != null) return connect!.future;
    if (method == 'send_presence' && presence != null) return presence!.future;
    if (method == 'ping_server' && ping != null) return ping!.future;
    return switch (method) {
      'discover_topology' => {'channels': <dynamic>[]},
      'fetch_inbox' => {'conversations': <dynamic>[]},
      'fetch_user_bookmarks' || 'fetch_dm_bookmarks' => <dynamic>[],
      'timeline_reset' => {'items': <dynamic>[]},
      _ => null,
    };
  }

  @override
  Future<void> close() async => closed = true;

  void connected() => controller.add({'type': 'connected', 'data': null});
}

Future<void> _until(bool Function() condition) async {
  for (var index = 0; index < 150; index++) {
    if (condition()) return;
    await Future<void>.delayed(const Duration(milliseconds: 2));
  }
  fail('The connection did not settle');
}

Future<void> _flush() async {
  for (var index = 0; index < 8; index++) {
    await Future<void>.delayed(Duration.zero);
  }
}

void main() {
  for (final reason in ['event', 'error', 'done', 'resync']) {
    test(
      '$reason disconnects dependents once before replacing the stream',
      () async {
        final handles = <_Transport>[];
        final events = <String>[];
        final runtime = SessionRuntime(
          storage: _Storage(),
          auth: _Auth(),
          reconnectDelay: (_) => Duration.zero,
          transportFactory: () async {
            final handle = _Transport();
            handles.add(handle);
            return handle;
          },
          platformInvoker: (_, _) async => null,
          rawEventObserver: (event) => events.add('${event['type']}'),
        );
        addTearDown(() async {
          await runtime.dispose();
          for (final handle in handles) {
            if (!handle.controller.isClosed) await handle.controller.close();
          }
        });
        await runtime.start();
        await _until(
          () => handles.isNotEmpty && handles.first.calls.contains('connect'),
        );
        final old = handles.single;
        old.connected();
        await _until(() => old.calls.contains('discover_topology'));
        switch (reason) {
          case 'event':
            old.controller.add({'type': 'disconnected', 'data': null});
          case 'error':
            old.controller.addError(StateError('poll failed'));
          case 'done':
            await old.controller.close();
          case 'resync':
            old.controller.add({
              'type': 'error',
              'data': {'code': 'resync_required'},
            });
        }
        await _until(() => handles.length == 2);
        expect(old.closed, isTrue);
        expect(events.where((type) => type == 'disconnected'), hasLength(1));
        if (!old.controller.isClosed) {
          old.controller.add({'type': 'disconnected', 'data': null});
        }
        await _flush();
        expect(handles, hasLength(2));
        expect(events.where((type) => type == 'disconnected'), hasLength(1));
      },
    );
  }

  test(
    'failed initial presence reconnects while the stream remains open',
    () async {
      final handles = <_Transport>[];
      final rawEvents = <String>[];
      final runtimeEvents = <Map<String, dynamic>>[];
      final runtime = SessionRuntime(
        storage: _Storage(),
        auth: _Auth(),
        reconnectDelay: (_) => Duration.zero,
        transportFactory: () async {
          final handle = _Transport();
          if (handles.isEmpty) handle.presence = Completer<dynamic>();
          handles.add(handle);
          return handle;
        },
        platformInvoker: (_, _) async => null,
        rawEventObserver: (event) => rawEvents.add('${event['type']}'),
      );
      final subscription = runtime.events.listen(runtimeEvents.add);
      addTearDown(() async {
        await subscription.cancel();
        await runtime.dispose();
        for (final handle in handles) {
          if (!handle.controller.isClosed) await handle.controller.close();
        }
      });
      await runtime.start();
      await _until(
        () => handles.isNotEmpty && handles.first.calls.contains('connect'),
      );
      final old = handles.single;
      old.connected();
      await _until(() => old.calls.contains('send_presence'));
      old.presence!.completeError(
        const RustTransportException('not_connected', 'disconnected'),
      );
      await _until(() => handles.length == 2);
      expect(old.controller.isClosed, isFalse);
      expect(old.closed, isTrue);
      expect(rawEvents.where((type) => type == 'disconnected'), hasLength(1));
      expect(
        runtimeEvents.where((event) => event['topic'] == 'error'),
        isEmpty,
      );
      expect(
        runtimeEvents.where(
          (event) =>
              event['topic'] == 'connection' &&
              (event['data'] as Map)['state'] == 'reconnecting',
        ),
        isNotEmpty,
      );
      handles.last.connected();
      await _until(() => handles.last.calls.contains('discover_topology'));
      expect(handles.last.closed, isFalse);
    },
  );

  test(
    'a late failed presence cannot retire a replacement connection',
    () async {
      final handles = <_Transport>[];
      final events = <String>[];
      final runtimeEvents = <Map<String, dynamic>>[];
      final runtime = SessionRuntime(
        storage: _Storage(),
        auth: _Auth(),
        reconnectDelay: (_) => Duration.zero,
        transportFactory: () async {
          final handle = _Transport();
          if (handles.isEmpty) handle.presence = Completer<dynamic>();
          handles.add(handle);
          return handle;
        },
        platformInvoker: (_, _) async => null,
        rawEventObserver: (event) => events.add('${event['type']}'),
      );
      final subscription = runtime.events.listen(runtimeEvents.add);
      addTearDown(() async {
        await subscription.cancel();
        await runtime.dispose();
        for (final handle in handles) {
          if (!handle.controller.isClosed) await handle.controller.close();
        }
      });
      await runtime.start();
      await _until(
        () => handles.isNotEmpty && handles.first.calls.contains('connect'),
      );
      final old = handles.single;
      old.connected();
      await _until(() => old.calls.contains('send_presence'));
      old.controller.addError(StateError('poll failed'));
      await _until(() => handles.length == 2);
      handles.last.connected();
      await _until(() => handles.last.calls.contains('discover_topology'));
      old.presence!.completeError(
        const RustTransportException(
          'transport_error',
          'old connection failed',
        ),
      );
      await _flush();
      expect(old.calls, isNot(contains('discover_topology')));
      expect(handles, hasLength(2));
      expect(handles.last.closed, isFalse);
      expect(events.where((type) => type == 'disconnected'), hasLength(1));
      expect(
        runtimeEvents.where((event) => event['topic'] == 'error'),
        isEmpty,
      );
    },
  );

  test('a failed pending connect settles before the retry starts', () async {
    final handles = <_Transport>[];
    final runtime = SessionRuntime(
      storage: _Storage(),
      auth: _Auth(),
      reconnectDelay: (_) => Duration.zero,
      transportFactory: () async {
        final handle = _Transport();
        if (handles.isEmpty) handle.connect = Completer<dynamic>();
        handles.add(handle);
        return handle;
      },
      platformInvoker: (_, _) async => null,
    );
    addTearDown(() async {
      await runtime.dispose();
      for (final handle in handles) {
        if (!handle.controller.isClosed) await handle.controller.close();
      }
    });
    await runtime.start();
    await _until(
      () => handles.isNotEmpty && handles.first.calls.contains('connect'),
    );
    handles.first.controller.addError(StateError('poll failed'));
    await _flush();
    expect(handles, hasLength(1));
    handles.first.connect!.complete();
    await _until(() => handles.length == 2);
    expect(handles.first.closed, isTrue);
  });

  test('logout fences a retry waiting for disconnect persistence', () async {
    final handles = <_Transport>[];
    final storage = _Storage();
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
    addTearDown(() async {
      await runtime.dispose();
      for (final handle in handles) {
        if (!handle.controller.isClosed) await handle.controller.close();
      }
    });
    await runtime.start();
    await _until(
      () => handles.isNotEmpty && handles.first.calls.contains('connect'),
    );
    handles.single.connected();
    await _until(() => handles.single.calls.contains('discover_topology'));
    storage.accountWrite = Completer<void>();
    handles.single.controller.addError(StateError('poll failed'));
    await _flush();
    final logout = runtime.invoke('session.logout', {});
    await _flush();
    expect(handles, hasLength(1));
    storage.accountWrite!.complete();
    await logout;
    await _flush();
    expect(runtime.session['state'], 'signedOut');
    expect(handles, hasLength(1));
  });

  test('a failed ping disconnects dependents and starts a retry', () async {
    await runZoned(
      () async {
        final handles = <_Transport>[];
        final events = <String>[];
        final runtime = SessionRuntime(
          storage: _Storage(),
          auth: _Auth(),
          reconnectDelay: (_) => Duration.zero,
          transportFactory: () async {
            final handle = _Transport();
            if (handles.isEmpty) handle.ping = Completer<dynamic>();
            handles.add(handle);
            return handle;
          },
          platformInvoker: (_, _) async => null,
          rawEventObserver: (event) => events.add('${event['type']}'),
        );
        addTearDown(() async {
          await runtime.dispose();
          for (final handle in handles) {
            if (!handle.controller.isClosed) await handle.controller.close();
          }
        });
        await runtime.start();
        await _until(
          () => handles.isNotEmpty && handles.first.calls.contains('connect'),
        );
        handles.first.connected();
        await _until(() => handles.first.calls.contains('ping_server'));
        handles.first.ping!.completeError(StateError('probe failed'));
        await _until(() => handles.length == 2);
        expect(handles.first.closed, isTrue);
        expect(events.where((type) => type == 'disconnected'), hasLength(1));
      },
      zoneSpecification: ZoneSpecification(
        createPeriodicTimer: (self, parent, zone, duration, callback) =>
            parent.createPeriodicTimer(
              zone,
              duration == const Duration(seconds: 180)
                  ? const Duration(milliseconds: 5)
                  : duration,
              callback,
            ),
      ),
    );
  });

  test('a stale failed ping cannot retire the replacement stream', () async {
    await runZoned(
      () async {
        final handles = <_Transport>[];
        final runtime = SessionRuntime(
          storage: _Storage(),
          auth: _Auth(),
          reconnectDelay: (_) => Duration.zero,
          transportFactory: () async {
            final handle = _Transport();
            if (handles.isEmpty) handle.ping = Completer<dynamic>();
            handles.add(handle);
            return handle;
          },
          platformInvoker: (_, _) async => null,
        );
        addTearDown(() async {
          await runtime.dispose();
          for (final handle in handles) {
            if (!handle.controller.isClosed) await handle.controller.close();
          }
        });
        await runtime.start();
        await _until(
          () => handles.isNotEmpty && handles.first.calls.contains('connect'),
        );
        final old = handles.single;
        old.connected();
        await _until(() => old.calls.contains('ping_server'));
        old.controller.add({'type': 'disconnected', 'data': null});
        await _until(() => handles.length == 2);
        handles.last.connected();
        await _until(() => handles.last.calls.contains('discover_topology'));
        old.ping!.completeError(StateError('old probe failed'));
        await _flush();
        expect(handles, hasLength(2));
        expect(handles.last.closed, isFalse);
      },
      zoneSpecification: ZoneSpecification(
        createPeriodicTimer: (self, parent, zone, duration, callback) =>
            parent.createPeriodicTimer(
              zone,
              duration == const Duration(seconds: 180)
                  ? const Duration(milliseconds: 5)
                  : duration,
              callback,
            ),
      ),
    );
  });

  test(
    'a connection timeout retires the failed attempt before retry',
    () async {
      await runZoned(
        () async {
          final handles = <_Transport>[];
          final events = <String>[];
          final runtime = SessionRuntime(
            storage: _Storage(),
            auth: _Auth(),
            reconnectDelay: (_) => Duration.zero,
            transportFactory: () async {
              final handle = _Transport();
              handles.add(handle);
              return handle;
            },
            platformInvoker: (_, _) async => null,
            rawEventObserver: (event) => events.add('${event['type']}'),
          );
          addTearDown(() async {
            await runtime.dispose();
            for (final handle in handles) {
              if (!handle.controller.isClosed) await handle.controller.close();
            }
          });
          await runtime.start();
          await _until(
            () => handles.isNotEmpty && handles.first.calls.contains('connect'),
          );
          handles.first.connected();
          await _until(() => handles.first.calls.contains('discover_topology'));
          handles.first.controller.add({'type': 'disconnected', 'data': null});
          await _until(() => handles.length == 2);
          // The second connection never receives a connected event.
          await _until(() => handles.length == 3);
          expect(handles[1].closed, isTrue);
          expect(events.where((type) => type == 'disconnected'), hasLength(1));
        },
        zoneSpecification: ZoneSpecification(
          createTimer: (self, parent, zone, duration, callback) =>
              parent.createTimer(
                zone,
                duration == const Duration(seconds: 20)
                    ? const Duration(milliseconds: 25)
                    : duration,
                callback,
              ),
        ),
      );
    },
  );
}

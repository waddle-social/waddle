import 'dart:async';
import 'dart:convert';
import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_flutter/transport/native_bindings.dart';
import 'package:waddle_flutter/transport/transport.dart';
import 'package:waddle_flutter/transport/transport_native.dart';

class FakeBindings implements NativeRustBindings {
  int next = 0;
  bool destroyed = false, acknowledgeDisconnect = true;
  final queue = <Map<String, dynamic>>[];
  final requests = <int, Map<String, dynamic>>{};
  @override
  int create() => 7;
  @override
  int submit(int handle, String json) {
    final request = Map<String, dynamic>.from(jsonDecode(json) as Map);
    final id = ++next;
    requests[id] = request;
    if (request['method'] == 'disconnect' && acknowledgeDisconnect) {
      queue.add({
        'kind': 'result',
        'request_id': id,
        'generation': 3,
        'ok': true,
        'data': null,
      });
    }
    return id;
  }

  @override
  String poll(int handle) {
    final result = jsonEncode(queue);
    queue.clear();
    return result;
  }

  @override
  void destroy(int handle) {
    destroyed = true;
  }
}

void main() {
  test(
    'correlates out-of-order results without treating startup as connected',
    () async {
      final api = FakeBindings();
      final transport = NativeRustTransport(api, startPolling: false);
      final events = <Map<String, dynamic>>[];
      final subscription = transport.events.listen(events.add);
      final connect = transport.rpc('connect', {'jid': 'alice@example.test'});
      api.queue.add({
        'kind': 'result',
        'request_id': 1,
        'generation': 1,
        'ok': true,
        'data': null,
      });
      transport.poll();
      await connect;
      expect(events, isEmpty);
      final a = transport.rpc('fetch_vcard4', {'jid': 'a@example.test'});
      final b = transport.rpc('fetch_vcard4', {'jid': 'b@example.test'});
      api.queue.addAll([
        {
          'kind': 'result',
          'request_id': 3,
          'generation': 1,
          'ok': true,
          'data': {'nickname': 'B'},
        },
        {
          'kind': 'result',
          'request_id': 2,
          'generation': 1,
          'ok': true,
          'data': {'nickname': 'A'},
        },
        {
          'kind': 'event',
          'generation': 1,
          'sequence': 1,
          'event': {'type': 'connected', 'data': null},
        },
      ]);
      transport.poll();
      expect(await a, {'nickname': 'A'});
      expect(await b, {'nickname': 'B'});
      expect(events.single['type'], 'connected');
      final close = transport.close();
      transport.poll();
      await close;
      await subscription.cancel();
      expect(api.destroyed, isTrue);
    },
  );

  test(
    'requests submitted after connect before polling remain in the new generation',
    () async {
      final api = FakeBindings();
      final transport = NativeRustTransport(api, startPolling: false);
      final connect = transport.rpc('connect', {'jid': 'alice@example.test'});
      final request = transport.rpc('fetch_vcard4', {
        'jid': 'bob@example.test',
      });
      api.queue.addAll([
        {
          'kind': 'result',
          'request_id': 1,
          'generation': 1,
          'ok': true,
          'data': null,
        },
        {
          'kind': 'result',
          'request_id': 2,
          'generation': 1,
          'ok': true,
          'data': {'nickname': 'Bob'},
        },
      ]);
      transport.poll();
      await connect;
      expect(await request, {'nickname': 'Bob'});
      final close = transport.close();
      transport.poll();
      await close;
    },
  );

  test(
    'rejects retired requests and drops stale or duplicate events',
    () async {
      final api = FakeBindings();
      final transport = NativeRustTransport(api, startPolling: false);
      final events = <Map<String, dynamic>>[];
      final sub = transport.events.listen(events.add);
      final stale = transport.rpc('fetch_inbox');
      final failure = expectLater(
        stale,
        throwsA(
          isA<RustTransportException>().having(
            (e) => e.code,
            'code',
            'stale_session',
          ),
        ),
      );
      api.queue.addAll([
        {
          'kind': 'event',
          'generation': 2,
          'sequence': 2,
          'event': {'type': 'connected', 'data': null},
        },
        {
          'kind': 'result',
          'request_id': 1,
          'generation': 0,
          'ok': true,
          'data': {'body': 'old account'},
        },
        {
          'kind': 'event',
          'generation': 1,
          'sequence': 3,
          'event': {
            'type': 'message',
            'data': {'body': 'old account'},
          },
        },
        {
          'kind': 'event',
          'generation': 2,
          'sequence': 2,
          'event': {
            'type': 'message',
            'data': {'body': 'duplicate'},
          },
        },
      ]);
      transport.poll();
      await failure;
      expect(events.length, 1);
      final close = transport.close();
      transport.poll();
      await close;
      await sub.cancel();
    },
  );

  test(
    'propagates explicit native errors and closes pending futures',
    () async {
      final api = FakeBindings();
      final transport = NativeRustTransport(api, startPolling: false);
      final rejected = transport.rpc('unknown');
      final failure = expectLater(
        rejected,
        throwsA(
          isA<RustTransportException>().having(
            (e) => e.code,
            'code',
            'unsupported_method',
          ),
        ),
      );
      api.queue.add({
        'kind': 'result',
        'request_id': 1,
        'generation': 0,
        'ok': false,
        'error': {
          'code': 'unsupported_method',
          'message': 'Unsupported command',
        },
      });
      transport.poll();
      await failure;
      final pending = transport.rpc('fetch_inbox');
      final closed = expectLater(
        pending,
        throwsA(isA<RustTransportException>()),
      );
      final close = transport.close();
      transport.poll();
      await close;
      await closed;
      await expectLater(
        transport.rpc('connect'),
        throwsA(isA<RustTransportException>()),
      );
      expect(api.destroyed, isTrue);
    },
  );
  test(
    'failed close retains the native owner and can retry confirmed shutdown',
    () async {
      final api = FakeBindings()..acknowledgeDisconnect = false;
      final transport = NativeRustTransport(
        api,
        closeTimeout: const Duration(milliseconds: 20),
        pollInterval: const Duration(milliseconds: 1),
      );
      await expectLater(transport.close(), throwsA(isA<TimeoutException>()));
      expect(api.destroyed, isFalse);
      await expectLater(
        transport.rpc('connect'),
        throwsA(isA<RustTransportException>()),
      );
      api.acknowledgeDisconnect = true;
      await transport.close();
      expect(api.destroyed, isTrue);
      await transport.close();
    },
  );
}

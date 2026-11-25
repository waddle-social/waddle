import 'dart:async';

import 'package:flutter_test/flutter_test.dart';
import 'package:flutter/material.dart';
import 'package:waddle_flutter/bridge.dart';
import 'package:waddle_flutter/features/remote_communities.dart';
import 'package:waddle_flutter/runtime/surfaces_runtime.dart';
import 'package:waddle_flutter/runtime/context.dart';
import 'platform_test_context.dart';

void main() {
  const address = 'xmpp:spaces.other.test?;node=music';
  const room = 'stage@muc.other.test';
  testWidgets(
    'nested remote entries require independent lookup and explicit room admission',
    (tester) async {
      final calls = <String>[];
      final bridge = WaddleBridge(
        events: const Stream.empty(),
        commandInvoker: (method, args) async {
          calls.add(method);
          if (method == 'remote.saved') return [];
          if (method == 'remote.lookup' && args['address'] == address) {
            return {
              'kind': 'space',
              'address': address,
              'name': 'Music',
              'entries': [
                {'address': room, 'name': 'Stage'},
              ],
            };
          }
          if (method == 'remote.lookup' && args['address'] == room) {
            return {
              'kind': 'room',
              'address': room,
              'name': 'Stage',
              'entries': [],
            };
          }
          if (method == 'remote.join') {
            return {'joined': true, 'message': 'Joined the room.'};
          }
          throw StateError(method);
        },
      );
      addTearDown(bridge.dispose);
      await tester.pumpWidget(
        MaterialApp(home: RemoteCommunitiesScreen(bridge: bridge)),
      );
      await tester.pumpAndSettle();
      await tester.enterText(find.byType(TextField), address);
      await tester.tap(find.text('Look up address'));
      await tester.pumpAndSettle();
      expect(find.text('Follow Space'), findsOneWidget);
      await tester.tap(find.text('Stage'));
      await tester.pumpAndSettle();
      expect(
        calls,
        orderedEquals(['remote.saved', 'remote.lookup', 'remote.lookup']),
      );
      await tester.tap(find.text('Join room'));
      await tester.pumpAndSettle();
      expect(find.text('Open conversation'), findsOneWidget);
      expect(calls, contains('remote.join'));
    },
  );
  test(
    'a verified Space follow preserves the host subid across failed private saves without entering rooms',
    () async {
      final ctx = TestRuntimeContext();
      final controller = SurfaceRuntime(ctx);
      addTearDown(controller.dispose);
      bool failSave = true;
      ctx.handler = (method, args) async {
        switch (method) {
          case 'describe_remote_community':
            return {
              'kind': 'space',
              'address': address,
              'entries': [
                {'address': room},
              ],
            };
          case 'fetch_saved_remote_spaces':
            return [];
          case 'change_remote_subscription':
            return {'subid': 's-1', 'state': 'pending'};
          case 'update_saved_remote_space':
            if (failSave) throw StateError('private node unavailable');
            return null;
          default:
            throw StateError(method);
        }
      };
      await expectLater(
        controller.invoke('remote.follow', {'address': address}),
        throwsStateError,
      );
      await controller.invoke('remote.lookup', {'address': address});
      final followed = await controller.invoke('remote.follow', {
        'address': address,
      });
      expect(followed['message'], contains('reviewing'));
      expect(
        (await controller.invoke('remote.saved', {})).single['subid'],
        's-1',
      );
      failSave = false;
      await controller.invoke('remote.retrySave', {'address': address});
      expect(
        ctx.calls.where((e) => e['method'] == 'change_remote_subscription'),
        hasLength(1),
      );
      expect(
        ctx.calls.where((e) => e['method'] == 'request_remote_room_presence'),
        isEmpty,
      );
    },
  );
  test(
    'room joins await self-presence before bookmarking; leave keeps bookmark and access errors never bookmark',
    () async {
      final ctx = TestRuntimeContext();
      final controller = SurfaceRuntime(ctx);
      addTearDown(controller.dispose);
      var sent = Completer<void>();
      ctx.handler = (method, args) async {
        if (method == 'describe_remote_community') {
          return {'kind': 'room', 'address': room};
        }
        if (method == 'request_remote_room_presence') sent.complete();
        return null;
      };
      Future<void> presence(String from, String type, List<int> codes) =>
          controller.onEvent({
            'type': 'presence',
            'data': {
              'from': from,
              'presence_type': type,
              'muc_status_codes': codes,
              'error_condition': 'forbidden',
            },
          });
      await controller.invoke('remote.lookup', {'address': room});
      final join = controller.invoke('remote.join', {'address': room});
      await sent.future;
      await presence('$room/another', 'available', []);
      await presence('other@muc.other.test/alice', 'available', [110]);
      expect(
        ctx.calls.where((e) => e['method'] == 'update_remote_room_bookmark'),
        isEmpty,
      );
      await presence('$room/Alice1', 'available', [110, 210]);
      expect((await join)['joined'], isTrue);
      sent = Completer();
      final leave = controller.invoke('remote.leave', {'address': room});
      await sent.future;
      expect(ctx.calls.last['args']['nick'], 'Alice1');
      await presence('$room/Alice1', 'unavailable', [110]);
      expect((await leave)['joined'], isFalse);
      expect(ctx.calls.last['args'], containsPair('forget', false));
      expect(ctx.calls.last['args'], containsPair('autojoin', false));
      sent = Completer();
      final denied = expectLater(
        controller.invoke('remote.join', {'address': room}),
        throwsStateError,
      );
      await sent.future;
      await presence('$room/alice', 'error', []);
      await denied;
      expect(
        ctx.calls.where((e) => e['method'] == 'update_remote_room_bookmark'),
        hasLength(2),
      );
    },
  );
  test(
    'account change fences in-flight admissions, verified offers and private reads',
    () async {
      final ctx = TestRuntimeContext();
      final controller = SurfaceRuntime(ctx);
      addTearDown(controller.dispose);
      final sent = Completer<void>();
      ctx.handler = (method, args) async {
        if (method == 'describe_remote_community') {
          return {'kind': 'room', 'address': room};
        }
        if (method == 'request_remote_room_presence') sent.complete();
        return null;
      };
      await controller.invoke('remote.lookup', {'address': room});
      final join = expectLater(
        controller.invoke('remote.join', {'address': room}),
        throwsA(anyOf(isA<AccountChanged>(), isA<StateError>())),
      );
      await sent.future;
      ctx.generation++;
      controller.onAccountChanged();
      await join;
      await expectLater(
        controller.invoke('remote.join', {'address': room}),
        throwsStateError,
      );
      expect(
        ctx.calls.where((e) => e['method'] == 'update_remote_room_bookmark'),
        isEmpty,
      );
    },
  );
}

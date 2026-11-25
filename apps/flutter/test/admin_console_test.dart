import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_flutter/bridge.dart';
import 'package:waddle_flutter/features/surfaces.dart';
import 'package:waddle_flutter/runtime/surfaces_runtime.dart';

import 'platform_test_context.dart';

void main() {
  const space = {'space_jid': 'spaces.example', 'space_node': 's1'};
  const room = {'channel_jid': 'lobby@rooms.example'};
  const lobby = {
    ...room,
    'name': 'Lobby',
    'members_only': true,
    'channel_type': 'text',
    'is_public': false,
    'occupant_count': 1,
    'member_count': 2,
  };
  const account = 'alice@example';
  Map<String, dynamic> page(List<Map<String, dynamic>> rows, [String? next]) =>
      {'entries': rows, 'nextCursor': next};

  Future<WaddleBridge> open(
    WidgetTester tester,
    CommandInvoker command, {
    bool owner = true,
  }) async {
    final bridge = WaddleBridge(
      commandInvoker: command,
      events: const Stream.empty(),
    );
    addTearDown(bridge.dispose);
    bridge.acceptEvent({
      'topic': 'home',
      'generation': 1,
      'data': {'isCommunityOwner': owner},
    });
    bridge.acceptEvent({
      'topic': 'connection',
      'generation': 1,
      'data': {'state': 'ready'},
    });
    await tester.pumpWidget(
      MaterialApp(home: CommunityAdminScreen(bridge: bridge)),
    );
    await tester.pumpAndSettle();
    return bridge;
  }

  Future<void> tap(WidgetTester tester, String label) async {
    await tester.tap(find.text(label).last);
    await tester.pumpAndSettle();
  }

  test(
    'binds list records, compound space IDs and room moderation to the native schema',
    () async {
      final context = TestRuntimeContext()
        ..handler = (method, args) async => {
          'entries': [],
          'next_cursor': null,
        };
      final runtime = SurfaceRuntime(context);
      addTearDown(runtime.dispose);
      await runtime.invoke('admin.rooms', {
        ...space,
        'prefix': 'dev',
        'afterCursor': 'c',
      });
      expect(context.calls.last, {
        'method': 'admin_channels_list',
        'args': {
          'args': {
            ...space,
            'prefix': 'dev',
            'page_size': 50,
            'after_cursor': 'c',
          },
        },
      });
      await runtime.invoke('admin.setRole', {
        ...space,
        'member_jid': account,
        'role': 'admin',
      });
      expect(context.calls.last, {
        'method': 'admin_spaces_set_role',
        'args': {...space, 'member_jid': account, 'role': 'admin'},
      });
      await runtime.invoke('admin.kick', {
        ...room,
        'occupant_jid': account,
        'reason': ' Stop ',
      });
      expect(context.calls.last, {
        'method': 'admin_channels_kick',
        'args': {...room, 'occupant_jid': account, 'reason': 'Stop'},
      });
      await expectLater(
        runtime.invoke('admin.deleteSpace', {'space_jid': space['space_jid']}),
        throwsArgumentError,
      );
    },
  );

  testWidgets('admin route requires the active account to be an owner', (
    tester,
  ) async {
    var requests = 0;
    final bridge = await open(tester, (method, args) async {
      requests++;
      return page([]);
    }, owner: false);
    expect(requests, 0);
    expect(find.text('Community owner access is required.'), findsOneWidget);
    bridge.acceptEvent({
      'topic': 'home',
      'generation': 1,
      'data': {'isCommunityOwner': true},
    });
    await tester.pumpAndSettle();
    expect(requests, 1);
    bridge.acceptEvent({
      'topic': 'session',
      'generation': 2,
      'data': {'state': 'ready'},
    });
    await tester.pumpAndSettle();
    expect(find.text('Spaces'), findsNothing);
    expect(requests, 1);
  });

  testWidgets(
    'space pages retain nodes on a shared service and confirm cascade deletion',
    (tester) async {
      final deleted = <Map<String, dynamic>>[];
      Map<String, dynamic> entry(String node, String name) => {
        ...space,
        'space_node': node,
        'name': name,
        'channel_count': 2,
        'member_count': 3,
      };
      await open(tester, (method, args) async {
        if (method == 'admin.spaces') {
          if (args['afterCursor'] == 'next') {
            return page([entry('s1', 'Oak'), entry('s2', 'Elm')], 'next');
          }
          return page([entry('s1', 'Oak')], 'next');
        }
        if (method == 'admin.deleteSpace') deleted.add(args);
        return page([]);
      });
      await tap(tester, 'Spaces');
      await tap(tester, 'Load more');
      expect(find.text('Oak'), findsOneWidget);
      expect(find.text('Elm'), findsOneWidget);
      expect(find.text('Load more'), findsNothing);
      await tap(tester, 'Elm');
      await tester.tap(find.byType(PopupMenuButton<String>));
      await tester.pumpAndSettle();
      await tap(tester, 'Delete space');
      expect(find.textContaining('and its rooms'), findsOneWidget);
      await tap(tester, 'Cancel');
      expect(deleted, isEmpty);
      await tester.tap(find.byType(PopupMenuButton<String>));
      await tester.pumpAndSettle();
      await tap(tester, 'Delete space');
      await tap(tester, 'Confirm');
      expect(deleted, [
        {...space, 'space_node': 's2'},
      ]);
    },
  );

  testWidgets(
    'occupant removal names membership consequences and sends the bare account plus reason',
    (tester) async {
      final kicked = <Map<String, dynamic>>[];
      await open(tester, (method, args) async {
        if (method == 'admin.rooms') return page([lobby]);
        if (method == 'admin.occupants') {
          return page([
            {
              'real_jid': '$account/phone',
              'nick': 'Alice',
              'role': 'participant',
              'affiliation': 'member',
            },
          ]);
        }
        if (method == 'admin.kick') kicked.add(args);
        return page([]);
      });
      await tap(tester, 'Rooms');
      await tap(tester, 'Lobby');
      await tap(tester, 'Alice');
      await tap(tester, 'Remove from room');
      expect(find.textContaining('end their membership'), findsOneWidget);
      expect(kicked, isEmpty);
      await tester.enterText(find.byType(TextField), 'Moderation');
      await tap(tester, 'Confirm');
      expect(kicked, [
        {...room, 'occupant_jid': account, 'reason': 'Moderation'},
      ]);
    },
  );
}

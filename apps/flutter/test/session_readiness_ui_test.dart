import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_flutter/bridge.dart';
import 'package:waddle_flutter/features/surfaces.dart';
import 'package:waddle_flutter/transport/transport.dart';

void main() {
  testWidgets(
    'profile waits for readiness and retries after a lost connection',
    (tester) async {
      final first = Completer<dynamic>();
      var calls = 0;
      final bridge = WaddleBridge(
        events: const Stream<dynamic>.empty(),
        commandInvoker: (method, args) async {
          if (method == 'profile.load') {
            calls++;
            if (calls == 1) return first.future;
            return {'fullName': 'Connected user'};
          }
          return null;
        },
      );
      addTearDown(bridge.dispose);
      bridge.acceptEvent({
        'topic': 'session',
        'data': {'state': 'ready', 'jid': 'me@example.org'},
      });
      bridge.acceptEvent({
        'topic': 'connection',
        'data': {'state': 'connecting'},
      });
      await tester.pumpWidget(MaterialApp(home: ProfileScreen(bridge: bridge)));
      expect(calls, 0);
      expect(find.byType(CircularProgressIndicator), findsOneWidget);

      bridge.acceptEvent({
        'topic': 'connection',
        'data': {'state': 'ready'},
      });
      await tester.pump();
      expect(calls, 1);
      bridge.acceptEvent({
        'topic': 'connection',
        'data': {'state': 'reconnecting'},
      });
      bridge.acceptEvent({
        'topic': 'connection',
        'data': {'state': 'ready'},
      });
      await tester.pump();
      expect(calls, 1);
      first.completeError(
        const RustTransportException('not_connected', 'Connection dropped'),
      );
      await tester.pump();
      await tester.pump();
      expect(calls, 2);
      expect(find.text('Could not load this profile.'), findsNothing);
      expect(find.byType(CircularProgressIndicator), findsNothing);
      expect(bridge.error, isNull);
      await tester.pumpWidget(const SizedBox.shrink());
    },
  );

  testWidgets('community users wait for readiness and ignore an old result', (
    tester,
  ) async {
    final first = Completer<dynamic>();
    var calls = 0;
    final bridge = WaddleBridge(
      events: const Stream<dynamic>.empty(),
      commandInvoker: (method, args) async {
        if (method == 'admin.users') {
          calls++;
          if (calls == 1) return first.future;
          return {
            'users': [
              {'jid': 'new@example.org', 'name': 'New user'},
            ],
          };
        }
        return null;
      },
    );
    addTearDown(bridge.dispose);
    bridge.acceptEvent({
      'topic': 'connection',
      'data': {'state': 'connecting'},
    });
    await tester.pumpWidget(
      MaterialApp(home: CommunityAdminScreen(bridge: bridge)),
    );
    expect(calls, 0);
    bridge.acceptEvent({
      'topic': 'connection',
      'data': {'state': 'ready'},
    });
    await tester.pump();
    expect(calls, 1);
    bridge.acceptEvent({
      'topic': 'connection',
      'data': {'state': 'reconnecting'},
    });
    bridge.acceptEvent({
      'topic': 'connection',
      'data': {'state': 'ready'},
    });
    await tester.pump();
    await tester.pump();
    expect(calls, 2);
    first.complete({
      'users': [
        {'jid': 'old@example.org', 'name': 'Old user'},
      ],
    });
    await tester.pump();
    expect(find.text('New user'), findsOneWidget);
    expect(find.text('Old user'), findsNothing);
    expect(bridge.error, isNull);
    await tester.pumpWidget(const SizedBox.shrink());
  });
}

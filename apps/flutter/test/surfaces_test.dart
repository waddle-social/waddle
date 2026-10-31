import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter/scheduler.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_flutter/app.dart';
import 'package:waddle_flutter/bridge.dart';
import 'package:waddle_flutter/features/surfaces.dart';

void main() {
  testWidgets('room dialog retains its controllers through closing animation', (
    tester,
  ) async {
    final bridge = WaddleBridge(
      events: const Stream<dynamic>.empty(),
      commandInvoker: (method, arguments) async => null,
    );
    addTearDown(bridge.dispose);
    bridge.acceptEvent({
      'topic': 'session',
      'data': {'state': 'ready', 'jid': 'me@example.org'},
    });
    bridge.acceptEvent({
      'topic': 'home',
      'data': {'canCreate': true, 'settled': true},
    });
    await tester.pumpWidget(MaterialApp(home: WaddleShell(bridge: bridge)));
    await tester.tap(find.text('New room'));
    await tester.pumpAndSettle();
    final dialog = find.byType(AlertDialog);
    await tester.enterText(
      find.descendant(of: dialog, matching: find.byType(TextField)).first,
      'A new room',
    );
    await tester.tap(find.widgetWithText(TextButton, 'Cancel'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));
    expect(tester.takeException(), isNull);
    await tester.pumpAndSettle();
    expect(find.byType(AlertDialog), findsNothing);
    await tester.pumpWidget(const SizedBox.shrink());
  });

  testWidgets(
    'group dialog owns controllers until a slow transition completes',
    (tester) async {
      final previousTimeDilation = timeDilation;
      timeDilation = 10;
      addTearDown(() => timeDilation = previousTimeDilation);
      final bridge = WaddleBridge(
        events: const Stream<dynamic>.empty(),
        commandInvoker: (method, arguments) async => null,
      );
      addTearDown(bridge.dispose);
      bridge.acceptEvent({
        'topic': 'home',
        'data': {
          'people': [
            {'jid': 'alex@example.org', 'name': 'Alex'},
          ],
        },
      });
      await tester.pumpWidget(
        MaterialApp(
          home: PeopleScreen(bridge: bridge, onMessage: (_, _) {}),
        ),
      );
      await tester.tap(find.byTooltip('New group message'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('Alex').last);
      await tester.pumpAndSettle();
      await tester.tap(find.text('Create · 1'));
      await tester.pumpAndSettle();
      await tester.enterText(
        find.descendant(
          of: find.byType(AlertDialog),
          matching: find.byType(TextField),
        ),
        'Friends',
      );
      await tester.tap(find.widgetWithText(TextButton, 'Cancel'));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 400));
      // This exceeds the former disposal delay while the slowed route remains.
      expect(find.byType(AlertDialog), findsOneWidget);
      await tester.pump(const Duration(milliseconds: 50));
      expect(tester.takeException(), isNull);
      await tester.pumpAndSettle();
      expect(find.byType(AlertDialog), findsNothing);
      timeDilation = previousTimeDilation;
      await tester.pumpWidget(const SizedBox.shrink());
    },
  );
  testWidgets('call controls retain the SID shown when their frame was built', (
    tester,
  ) async {
    final commands = <(String, Map<String, dynamic>)>[];
    final bridge = WaddleBridge(
      events: const Stream<dynamic>.empty(),
      commandInvoker: (method, args) async {
        commands.add((method, args));
        return null;
      },
    );
    addTearDown(bridge.dispose);
    bridge.acceptEvent({
      'topic': 'call',
      'data': {
        'state': 'active',
        'sid': 'first',
        'peerJid': 'alex@example.org',
        'video': false,
      },
    });
    await tester.pumpWidget(
      MaterialApp(home: ActiveCallScreen(bridge: bridge)),
    );
    final oldEnd = tester
        .widget<FilledButton>(find.widgetWithText(FilledButton, 'End call'))
        .onPressed!;
    bridge.acceptEvent({
      'topic': 'call',
      'data': {
        'state': 'incoming',
        'sid': 'successor',
        'peerJid': 'blair@example.org',
        'video': false,
      },
    });
    oldEnd();
    await tester.pump();
    expect(commands.where((row) => row.$1 == 'call.hangup').single.$2, {
      'expectedSid': 'first',
    });
    expect(find.text('Answer call'), findsOneWidget);
    await tester.pumpWidget(const SizedBox.shrink());
  });

  testWidgets('video calls use the portable renderer outside Android', (
    tester,
  ) async {
    debugDefaultTargetPlatformOverride = TargetPlatform.linux;
    final bridge = WaddleBridge(
      events: const Stream<dynamic>.empty(),
      commandInvoker: (method, args) async => null,
      callVideoBuilder: (context, local) =>
          SizedBox(key: ValueKey(local ? 'local-video' : 'remote-video')),
    );
    try {
      bridge.acceptEvent({
        'topic': 'call',
        'data': {
          'state': 'active',
          'sid': 'video-call',
          'peerJid': 'alex@example.org',
          'video': true,
          'cameraEnabled': true,
        },
      });
      await tester.pumpWidget(
        MaterialApp(home: ActiveCallScreen(bridge: bridge)),
      );
      expect(find.byKey(const ValueKey('remote-video')), findsOneWidget);
      expect(find.byKey(const ValueKey('local-video')), findsOneWidget);
      await tester.pumpWidget(const SizedBox.shrink());
    } finally {
      bridge.dispose();
      debugDefaultTargetPlatformOverride = null;
    }
  });
}

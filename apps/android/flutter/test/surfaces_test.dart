import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/scheduler.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_android/app.dart';
import 'package:waddle_android/bridge.dart';
import 'package:waddle_android/features/surfaces.dart';

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
}

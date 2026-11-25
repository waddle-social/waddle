import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter/scheduler.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_flutter/app.dart';
import 'package:waddle_flutter/bridge.dart';
import 'package:waddle_flutter/features/conversation.dart';
import 'package:waddle_flutter/features/surfaces.dart';

void main() {
  testWidgets('room dialog retains its controllers through closing animation', (
    tester,
  ) async {
    final created = <Map<String, dynamic>>[];
    final bridge = WaddleBridge(
      events: const Stream<dynamic>.empty(),
      commandInvoker: (method, arguments) async {
        if (method == 'room.create') {
          created.add(arguments);
          return {'jid': 'new@rooms.example.org'};
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
      'topic': 'home',
      'data': {'canCreate': true, 'settled': true},
    });
    await tester.pumpWidget(MaterialApp(home: WaddleShell(bridge: bridge)));
    await tester.tap(find.byTooltip('Browse all rooms'));
    await tester.pumpAndSettle();
    await tester.tap(find.byTooltip('New room'));
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
    expect(created, isEmpty);
    await tester.tap(find.byTooltip('New room'));
    await tester.pumpAndSettle();
    await tester.enterText(
      find.widgetWithText(TextField, 'Room name'),
      'A new room',
    );
    await tester.tap(find.widgetWithText(FilledButton, 'Create'));
    await tester.pumpAndSettle();
    expect(created.single, {
      'name': 'A new room',
      'description': '',
      'forum': false,
    });
    expect(
      tester.widget<ConversationScreen>(find.byType(ConversationScreen)).jid,
      'new@rooms.example.org',
    );
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

  testWidgets('call device picker and screen share act on the displayed call', (
    tester,
  ) async {
    final commands = <(String, Map<String, dynamic>)>[];
    final bridge = WaddleBridge(
      events: const Stream<dynamic>.empty(),
      commandInvoker: (method, args) async {
        commands.add((method, args));
        return method == 'call.devices'
            ? [
                {'kind': 'audiooutput', 'id': 'usb', 'label': 'Headset'},
              ]
            : null;
      },
      callVideoBuilder: (_, _) => const SizedBox(key: Key('presentation')),
    );
    addTearDown(bridge.dispose);
    bridge.acceptEvent({
      'topic': 'call',
      'data': {
        'state': 'active',
        'sid': 'voice',
        'video': false,
        'screenShareSupported': true,
        'remoteScreenShare': true,
      },
    });
    await tester.pumpWidget(
      MaterialApp(home: ActiveCallScreen(bridge: bridge)),
    );
    expect(find.byKey(const Key('presentation')), findsOneWidget);
    final devices = find.byTooltip('Devices');
    await tester.ensureVisible(devices);
    await tester.tap(devices);
    await tester.pumpAndSettle();
    await tester.tap(find.text('Headset'));
    await tester.pumpAndSettle();
    expect(commands.last.$1, 'call.selectDevice');
    expect(commands.last.$2, {
      'expectedSid': 'voice',
      'kind': 'audiooutput',
      'id': 'usb',
    });
    final share = find.byTooltip('Share screen');
    await tester.ensureVisible(share);
    await tester.tap(share);
    await tester.pump();
    expect(commands.last.$1, 'call.screenShare');
    expect(commands.last.$2, {'expectedSid': 'voice'});
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
      expect(find.byKey(const ValueKey('local-video')), findsNothing);
      await tester.pumpWidget(const SizedBox.shrink());
    } finally {
      bridge.dispose();
      debugDefaultTargetPlatformOverride = null;
    }
  });
}

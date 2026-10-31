import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_android/app.dart';
import 'package:waddle_android/bridge.dart';
import 'package:waddle_android/features/conversation.dart';
import 'package:waddle_android/features/surfaces.dart';

void main() {
  testWidgets('incoming calls appear over settings and present once per call', (
    tester,
  ) async {
    final bridge = WaddleBridge(
      events: const Stream<dynamic>.empty(),
      commandInvoker: (method, args) async => null,
    );
    addTearDown(bridge.dispose);
    bridge.acceptEvent({
      'topic': 'session',
      'data': {'state': 'ready', 'jid': 'me@example.org'},
    });
    bridge.acceptEvent({
      'topic': 'home',
      'data': {'settled': true},
    });
    await tester.pumpWidget(
      MaterialApp(
        restorationScopeId: 'test',
        home: WaddleShell(bridge: bridge),
      ),
    );
    await tester.tap(find.byTooltip('Settings'));
    await tester.pumpAndSettle();
    expect(find.byType(SettingsScreen), findsOneWidget);
    bridge.acceptEvent({
      'topic': 'call',
      'data': {
        'state': 'incoming',
        'sid': 'first',
        'peerJid': 'alex@example.org',
        'video': false,
      },
    });
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    expect(find.byType(ActiveCallScreen), findsOneWidget);
    await tester.binding.handlePopRoute();
    await tester.pumpAndSettle();
    bridge.acceptEvent({
      'topic': 'call',
      'data': {
        'state': 'incoming',
        'sid': 'first',
        'peerJid': 'alex@example.org',
        'video': false,
      },
    });
    await tester.pump();
    expect(find.byType(ActiveCallScreen), findsNothing);
    bridge.acceptEvent({
      'topic': 'call',
      'data': {
        'state': 'incoming',
        'sid': 'successor',
        'peerJid': 'blair@example.org',
        'video': false,
      },
    });
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    expect(find.byType(ActiveCallScreen), findsOneWidget);
    await tester.pumpWidget(const SizedBox.shrink());
  });

  testWidgets(
    'active conversation restores after Android process restoration',
    (tester) async {
      final bridge = WaddleBridge(
        events: const Stream<dynamic>.empty(),
        commandInvoker: (method, args) async => null,
      );
      addTearDown(bridge.dispose);
      tester.view.physicalSize = const Size(400, 900);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.resetPhysicalSize);
      addTearDown(tester.view.resetDevicePixelRatio);
      bridge.acceptEvent({
        'topic': 'session',
        'data': {'state': 'ready', 'jid': 'me@example.org'},
      });
      bridge.acceptEvent({
        'topic': 'home',
        'data': {
          'settled': true,
          'rooms': [
            {'jid': 'general@example.org', 'name': 'General'},
          ],
        },
      });
      await tester.pumpWidget(
        MaterialApp(
          restorationScopeId: 'test',
          home: WaddleShell(bridge: bridge),
        ),
      );
      await tester.tap(find.text('General'));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 400));
      expect(find.byType(ConversationScreen), findsOneWidget);
      final conversationState = tester.state(find.byType(ConversationScreen));
      await tester.enterText(find.byType(TextField), 'Draft before folding');
      tester.view.physicalSize = const Size(1200, 900);
      await tester.pump();
      expect(
        tester.state(find.byType(ConversationScreen)),
        same(conversationState),
      );
      expect(find.text('Draft before folding'), findsOneWidget);
      tester.view.physicalSize = const Size(400, 900);
      await tester.pump();
      expect(
        tester.state(find.byType(ConversationScreen)),
        same(conversationState),
      );
      bridge.acceptEvent({
        'topic': 'navigate',
        'data': {
          'jid': 'general@example.org',
          'title': 'General',
          'group': true,
        },
      });
      await tester.pump();
      expect(
        find.byType(ConversationScreen, skipOffstage: false),
        findsOneWidget,
      );
      expect(
        tester.state(find.byType(ConversationScreen)),
        same(conversationState),
      );
      await tester.restartAndRestore();
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 400));
      final restored = tester.widget<ConversationScreen>(
        find.byType(ConversationScreen),
      );
      expect(restored.jid, 'general@example.org');
      expect(restored.group, isTrue);
      expect(find.text('Draft before folding'), findsOneWidget);
      await tester.pumpWidget(const SizedBox.shrink());
    },
  );

  testWidgets('device authorization passes the chosen provider ID to Android', (
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
      'topic': 'session',
      'data': {'state': 'signedOut'},
    });
    bridge.acceptEvent({
      'topic': 'login',
      'data': {
        'state': 'providers',
        'providers': [
          {'id': 'community', 'name': 'Community'},
        ],
      },
    });
    await tester.pumpWidget(MaterialApp(home: WaddleShell(bridge: bridge)));
    await tester.tap(find.text('Continue with Community'));
    await tester.pump();
    expect(commands, contains(('login.select', {'id': 'community'})));
  });

  testWidgets(
    'phone and tablet navigation use Material 3 adaptive components',
    (tester) async {
      final bridge = WaddleBridge(
        events: const Stream<dynamic>.empty(),
        commandInvoker: (method, args) async => null,
      );
      addTearDown(bridge.dispose);
      bridge.acceptEvent({
        'topic': 'session',
        'data': {'state': 'ready', 'jid': 'me@example.org'},
      });
      bridge.acceptEvent({
        'topic': 'home',
        'data': {
          'rooms': [
            {'jid': 'room@example.org', 'name': 'General', 'unread': 2},
          ],
          'dmPeers': [],
          'people': [],
          'settled': true,
        },
      });
      tester.view.physicalSize = const Size(400, 900);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.resetPhysicalSize);
      addTearDown(tester.view.resetDevicePixelRatio);
      await tester.pumpWidget(
        MaterialApp(
          theme: ThemeData(useMaterial3: true),
          home: WaddleShell(bridge: bridge),
        ),
      );
      expect(find.byType(NavigationBar), findsOneWidget);
      expect(find.text('General'), findsOneWidget);
      tester.view.physicalSize = const Size(1200, 900);
      await tester.pumpAndSettle();
      expect(find.byType(NavigationRail), findsOneWidget);
      expect(find.byType(NavigationBar), findsNothing);
      expect(
        find.text('Choose a conversation to get started.'),
        findsOneWidget,
      );
    },
  );

  testWidgets(
    'appearance and privacy controls write existing native preferences',
    (tester) async {
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
        'topic': 'preferences',
        'data': {'wallpaperColors': false},
      });
      await tester.pumpWidget(
        MaterialApp(home: SettingsScreen(bridge: bridge)),
      );
      await tester.tap(find.widgetWithText(SwitchListTile, 'Wallpaper colors'));
      await tester.pump();
      expect(
        commands,
        contains((
          'preferences.set',
          {'key': 'wallpaperColors', 'value': true},
        )),
      );
    },
  );
}

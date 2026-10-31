import 'dart:async';
import 'dart:io';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_flutter/bridge.dart';
import 'package:waddle_flutter/design.dart';
import 'package:waddle_flutter/features/surfaces.dart';
import 'package:waddle_flutter/main.dart';

const _room = 'general@rooms.example.org';
const _previewDirectory = String.fromEnvironment('WADDLE_PREVIEW_DIR');

WaddleBridge _fixture(
  Brightness brightness, {
  bool social = false,
  bool wallpaper = false,
}) {
  final bridge = WaddleBridge(
    events: const Stream<dynamic>.empty(),
    commandInvoker: (method, arguments) async {
      if (method == 'conversation.mentionSearch') {
        return [
          {'jid': 'sam@example.org', 'name': 'Sam'},
        ];
      }
      return null;
    },
  );
  void event(String topic, Map<String, dynamic> data) =>
      bridge.acceptEvent({'topic': topic, 'data': data});
  event('session', {
    'state': 'ready',
    'jid': 'david@example.org',
    'username': 'David',
  });
  event('connection', {'state': 'ready'});
  event('preferences', {
    'theme': brightness.name,
    'timeline': social ? 'social' : 'chat',
    'wallpaperColors': wallpaper,
  });
  if (wallpaper) {
    event('palette', {
      'light': _wallpaperRoles(Brightness.light),
      'dark': _wallpaperRoles(Brightness.dark),
    });
  }
  event('profile', {
    'fullName': 'David',
    'nickname': 'David',
    'pronouns': 'he / him',
    'note': 'Good conversations make good company.',
    'url': 'https://waddle.social',
  });
  event('home', {
    'settled': true,
    'canCreate': true,
    'rooms': [
      {
        'jid': _room,
        'name': 'General',
        'description': 'A little bit of everything, and everyone.',
        'here': 3,
        'unread': 3,
      },
      {
        'jid': 'design@rooms.example.org',
        'name': 'Design',
        'description': 'Ideas worth making room for.',
        'unread': 1,
      },
      {
        'jid': 'music@rooms.example.org',
        'name': 'Music',
        'description': 'What have you got on repeat?',
      },
      {
        'jid': 'weekend@rooms.example.org',
        'name': 'Weekend plans',
        'description': 'Small plans. Good company.',
      },
    ],
    'dmPeers': [
      {
        'jid': 'alex@example.org',
        'name': 'Alex',
        'preview': 'See you at the bridge!',
        'unread': 2,
      },
    ],
    'people': [
      {'jid': 'alex@example.org', 'name': 'Alex', 'show': 'available'},
      {'jid': 'sam@example.org', 'name': 'Sam', 'show': 'away'},
      {'jid': 'blair@example.org', 'name': 'Blair', 'show': 'available'},
    ],
  });
  event('conversation', {
    'jid': _room,
    'loading': false,
    'reachedStart': true,
    'newestFirst': social,
    'canConfigure': true,
    'typing': <String>[],
    'messages': [
      {
        "id": "message-1",
        "body": "Morning! Is anyone around for a walk this weekend?",
        "author": "Alex",
        "authorJid": "alex@example.org",
        "timestamp": "2026-10-31T10:00:00Z",
        "startsGroup": true,
        "mine": false,
        "safetyScores": {
          "modelVersion": "fixture-1",
          "scores": [
            {"category": "Harassment", "probability": 0.01},
          ],
        },
        "replyCount": 6,
        "threadTargetId": "weekend-thread",
      },
      {
        "id": "message-2",
        "body": "Saturday works for me.",
        "author": "You",
        "authorJid": "david@example.org",
        "timestamp": "2026-10-31T10:01:00Z",
        "startsGroup": true,
        "mine": true,
        "safetyScores": {
          "modelVersion": "fixture-1",
          "scores": [
            {"category": "Harassment", "probability": 0.01},
          ],
        },
      },
      {
        "id": "message-3",
        "body":
            "There’s a good route along the river. We could stop for coffee on the way back.",
        "author": "You",
        "authorJid": "david@example.org",
        "timestamp": "2026-10-31T10:02:00Z",
        "startsGroup": false,
        "mine": true,
        "safetyScores": {
          "modelVersion": "fixture-1",
          "scores": [
            {"category": "Harassment", "probability": 0.01},
          ],
        },
      },
      {
        "id": "message-4",
        "body": "That sounds lovely. Count me in!",
        "author": "Sam",
        "authorJid": "sam@example.org",
        "timestamp": "2026-10-31T10:03:00Z",
        "startsGroup": true,
        "mine": false,
        "safetyScores": {
          "modelVersion": "fixture-1",
          "scores": [
            {"category": "Harassment", "probability": 0.01},
          ],
        },
      },
      {
        "id": "message-5",
        "body":
            "Could we make it a little later? I need to drop something off first.",
        "author": "Sam",
        "authorJid": "sam@example.org",
        "timestamp": "2026-10-31T10:04:00Z",
        "startsGroup": false,
        "mine": false,
        "safetyScores": {
          "modelVersion": "fixture-1",
          "scores": [
            {"category": "Harassment", "probability": 0.01},
          ],
        },
      },
      {
        "id": "message-6",
        "body": "Of course. How about 10:30 at the bridge?",
        "author": "Alex",
        "authorJid": "alex@example.org",
        "timestamp": "2026-10-31T10:05:00Z",
        "startsGroup": true,
        "mine": false,
        "safetyScores": {
          "modelVersion": "fixture-1",
          "scores": [
            {"category": "Harassment", "probability": 0.01},
          ],
        },
        "reactions": [
          {"emoji": "👍", "count": 3, "mine": true},
        ],
      },
      {
        "id": "message-7",
        "body": "Perfect. I’ll bring the dog, if that’s okay with everyone.",
        "author": "Blair",
        "authorJid": "blair@example.org",
        "timestamp": "2026-10-31T10:06:00Z",
        "startsGroup": true,
        "mine": false,
        "safetyScores": {
          "modelVersion": "fixture-1",
          "scores": [
            {"category": "Harassment", "probability": 0.01},
          ],
        },
      },
      {
        "id": "message-8",
        "body": "The more the merrier.",
        "author": "Alex",
        "authorJid": "alex@example.org",
        "timestamp": "2026-10-31T10:07:00Z",
        "startsGroup": true,
        "mine": false,
        "safetyScores": {
          "modelVersion": "fixture-1",
          "scores": [
            {"category": "Harassment", "probability": 0.01},
          ],
        },
      },
      {
        "id": "message-9",
        "body": "Done. See you there!",
        "author": "You",
        "authorJid": "david@example.org",
        "timestamp": "2026-10-31T10:08:00Z",
        "startsGroup": true,
        "mine": true,
        "safetyScores": {
          "modelVersion": "fixture-1",
          "scores": [
            {"category": "Harassment", "probability": 0.01},
          ],
        },
      },
    ],
  });
  return bridge;
}

Map<String, int> _wallpaperRoles(Brightness brightness) {
  final c = ColorScheme.fromSeed(
    seedColor: const Color(0xFF736DAB),
    brightness: brightness,
  );
  return {
    'primary': c.primary.toARGB32(),
    'onPrimary': c.onPrimary.toARGB32(),
    'primaryContainer': c.primaryContainer.toARGB32(),
    'onPrimaryContainer': c.onPrimaryContainer.toARGB32(),
    'primaryFixed': c.primaryFixed.toARGB32(),
    'primaryFixedDim': c.primaryFixedDim.toARGB32(),
    'onPrimaryFixed': c.onPrimaryFixed.toARGB32(),
    'onPrimaryFixedVariant': c.onPrimaryFixedVariant.toARGB32(),
    'inversePrimary': c.inversePrimary.toARGB32(),
    'secondary': c.secondary.toARGB32(),
    'onSecondary': c.onSecondary.toARGB32(),
    'secondaryContainer': c.secondaryContainer.toARGB32(),
    'onSecondaryContainer': c.onSecondaryContainer.toARGB32(),
    'secondaryFixed': c.secondaryFixed.toARGB32(),
    'secondaryFixedDim': c.secondaryFixedDim.toARGB32(),
    'onSecondaryFixed': c.onSecondaryFixed.toARGB32(),
    'onSecondaryFixedVariant': c.onSecondaryFixedVariant.toARGB32(),
    'tertiary': c.tertiary.toARGB32(),
    'onTertiary': c.onTertiary.toARGB32(),
    'tertiaryContainer': c.tertiaryContainer.toARGB32(),
    'onTertiaryContainer': c.onTertiaryContainer.toARGB32(),
    'tertiaryFixed': c.tertiaryFixed.toARGB32(),
    'tertiaryFixedDim': c.tertiaryFixedDim.toARGB32(),
    'onTertiaryFixed': c.onTertiaryFixed.toARGB32(),
    'onTertiaryFixedVariant': c.onTertiaryFixedVariant.toARGB32(),
    'surface': c.surface.toARGB32(),
    'onSurface': c.onSurface.toARGB32(),
    'onSurfaceVariant': c.onSurfaceVariant.toARGB32(),
    'surfaceDim': c.surfaceDim.toARGB32(),
    'surfaceBright': c.surfaceBright.toARGB32(),
    'surfaceContainerLowest': c.surfaceContainerLowest.toARGB32(),
    'surfaceContainerLow': c.surfaceContainerLow.toARGB32(),
    'surfaceContainer': c.surfaceContainer.toARGB32(),
    'surfaceContainerHigh': c.surfaceContainerHigh.toARGB32(),
    'surfaceContainerHighest': c.surfaceContainerHighest.toARGB32(),
    'surfaceTint': c.surfaceTint.toARGB32(),
    'inverseSurface': c.inverseSurface.toARGB32(),
    'onInverseSurface': c.onInverseSurface.toARGB32(),
    'outline': c.outline.toARGB32(),
    'outlineVariant': c.outlineVariant.toARGB32(),
    'shadow': c.shadow.toARGB32(),
    'scrim': c.scrim.toARGB32(),
    'error': c.error.toARGB32(),
    'onError': c.onError.toARGB32(),
    'errorContainer': c.errorContainer.toARGB32(),
    'onErrorContainer': c.onErrorContainer.toARGB32(),
  };
}

Future<void> _fonts() async {
  final manrope = FontLoader('Manrope');
  for (final weight in [400, 500, 600, 700, 800]) {
    manrope.addFont(rootBundle.load('assets/fonts/Manrope-$weight.ttf'));
  }
  await manrope.load();
  await (FontLoader(
    'Fredoka',
  )..addFont(rootBundle.load('assets/fonts/Fredoka-600.ttf'))).load();
  await (FontLoader(
    'MaterialIcons',
  )..addFont(rootBundle.load('fonts/MaterialIcons-Regular.otf'))).load();
}

Future<void> _capture(
  WidgetTester tester,
  GlobalKey boundaryKey,
  String name,
) async {
  if (_previewDirectory.isEmpty) return;
  final boundary =
      boundaryKey.currentContext!.findRenderObject()! as RenderRepaintBoundary;
  void repaint(RenderObject object) {
    object.visitChildren(repaint);
    object.markNeedsPaint();
  }

  repaint(boundary);
  await tester.pump();
  final imageFuture = boundary.toImage(pixelRatio: 2);
  await tester.runAsync(() async {
    final image = await imageFuture;
    final data = await image.toByteData(format: ui.ImageByteFormat.png);
    final file = File('$_previewDirectory/$name.png');
    await file.parent.create(recursive: true);
    await File('$_previewDirectory/README.txt').writeAsString(
      'Waddle Flutter design previews.\n'
      'Real widgets and bundled fonts, rendered at 2x pixel density with synthetic fixtures.\n'
      'Names encode screen, theme, logical width and text scale.\n'
      'These previews do not demonstrate live XMPP or device call behavior.\n',
    );
    await file.writeAsBytes(data!.buffer.asUint8List());
    image.dispose();
  });
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  setUpAll(_fonts);
  for (final brightness in Brightness.values) {
    for (final screen in [
      'home',
      'conversation',
      'conversation-social',
      'conversation-wallpaper',
      'settings',
      'profile',
      'call',
      'login',
    ]) {
      testWidgets(
        '$screen renders in ${brightness.name} with real bundled fonts',
        (tester) async {
          await _render(tester, screen, brightness, const Size(412, 915), 1);
        },
      );
    }
  }
  for (final screen in ['home', 'conversation']) {
    testWidgets('$screen adapts to tablet width', (tester) async {
      await _render(tester, screen, Brightness.dark, const Size(1280, 900), 1);
    });
  }
  for (final screen in [
    'home',
    'conversation',
    'conversation-social',
    'conversation-wallpaper',
    'settings',
    'profile',
    'call',
    'login',
  ]) {
    testWidgets('$screen fits 320px with double-size text', (tester) async {
      await _render(tester, screen, Brightness.light, const Size(320, 740), 2);
    });
  }
  for (final screen in [
    'conversation-keyboard',
    'conversation-social-keyboard',
  ]) {
    testWidgets(
      '$screen preserves a reply and selected mention above the keyboard',
      (tester) async {
        await _render(tester, screen, Brightness.dark, const Size(320, 740), 2);
      },
    );
  }
}

Future<void> _render(
  WidgetTester tester,
  String screen,
  Brightness brightness,
  Size size,
  double scale,
) async {
  tester.view.physicalSize = size;
  tester.view.devicePixelRatio = 1;
  final keyboard = screen.contains('keyboard');
  if (keyboard) {
    tester.view.viewInsets = const FakeViewPadding(bottom: 300);
    addTearDown(tester.view.resetViewInsets);
  }
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);
  final previousShadows = debugDisableShadows;
  debugDisableShadows = false;
  final conversation = screen.startsWith('conversation');
  final bridge = _fixture(
    brightness,
    social: screen.contains('social'),
    wallpaper: screen.contains('wallpaper'),
  );
  final ownedByApp = screen == 'home' || conversation || screen == 'login';
  final key = GlobalKey();
  if (screen == 'login') {
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
  }
  if (screen == 'call') {
    bridge.acceptEvent({
      'topic': 'call',
      'data': {
        'state': 'incoming',
        'sid': 'example-call',
        'peerName': 'Alex',
        'peerJid': 'alex@example.org',
        'video': false,
      },
    });
  }
  final page = switch (screen) {
    'settings' => SettingsScreen(bridge: bridge),
    'profile' => ProfileScreen(bridge: bridge),
    'call' => ActiveCallScreen(bridge: bridge),
    _ => const SizedBox.shrink(),
  };
  Widget scalable(BuildContext context, Widget? child) => MediaQuery(
    data: MediaQuery.of(context).copyWith(textScaler: TextScaler.linear(scale)),
    child: child!,
  );
  final app = ownedByApp
      ? MediaQuery(
          data: MediaQueryData.fromView(
            tester.view,
          ).copyWith(textScaler: TextScaler.linear(scale)),
          child: WaddleApp(bridge: bridge),
        )
      : MaterialApp(
          debugShowCheckedModeBanner: false,
          theme: waddleTheme(waddleScheme(brightness)),
          builder: scalable,
          home: page,
        );
  try {
    await tester.pumpWidget(RepaintBoundary(key: key, child: app));
    await tester.pumpAndSettle();
    await tester.runAsync(
      () => precacheImage(
        const AssetImage('assets/brand/waddle-mark.png'),
        tester.element(find.byType(Scaffold).first),
      ),
    );
    await tester.pumpAndSettle();
    if (conversation) {
      bridge.acceptEvent({
        'topic': 'navigate',
        'data': {'jid': _room, 'title': 'General', 'group': true},
      });
      await tester.pumpAndSettle();
    }
    if (keyboard) {
      final message = find.byKey(const ValueKey<dynamic>('message-9'));
      await tester.tap(
        find
            .descendant(
              of: message,
              matching: find.byTooltip('Message actions'),
            )
            .first,
      );
      await tester.pumpAndSettle();
      await tester.ensureVisible(find.text('Reply').last);
      await tester.tap(find.text('Reply').last);
      await tester.pumpAndSettle();
      await tester.enterText(find.byType(TextField).last, '@Sa');
      await tester.pump(const Duration(milliseconds: 250));
      await tester.pump();
      await tester.tap(find.text('Sam').last);
      await tester.pumpAndSettle();
      final editor = tester.widget<TextField>(find.byType(TextField).last);
      final text = '${editor.controller!.text}See you at the bridge.';
      tester.testTextInput.updateEditingValue(
        TextEditingValue(
          text: text,
          selection: TextSelection.collapsed(offset: text.length),
        ),
      );
      await tester.pumpAndSettle();
      expect(
        MediaQuery.viewInsetsOf(
          tester.element(find.byType(Scaffold).first),
        ).bottom,
        300,
      );
      expect(find.byTooltip('Cancel reply'), findsOneWidget);
      expect(
        tester.getBottomRight(find.byTooltip('Send message')).dy,
        lessThanOrEqualTo(440),
      );
    }
    if (conversation) expect(find.text('Safety scores'), findsNothing);
    expect(tester.takeException(), isNull);
    expect(
      MediaQuery.textScalerOf(
        tester.element(find.byType(Scaffold).first),
      ).scale(1),
      scale,
    );
    await _capture(
      tester,
      key,
      '${screen}_${brightness.name}_${size.width.toInt()}_${scale.toInt()}x',
    );
  } finally {
    debugDisableShadows = previousShadows;
    await tester.pumpWidget(const SizedBox.shrink());
    if (!ownedByApp) bridge.dispose();
  }
}

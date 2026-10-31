import 'dart:async';
import 'dart:io';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_android/bridge.dart';
import 'package:waddle_android/design.dart';
import 'package:waddle_android/features/surfaces.dart';
import 'package:waddle_android/main.dart';

const _room = 'general@rooms.example.org';
const _previewDirectory = String.fromEnvironment('WADDLE_PREVIEW_DIR');

WaddleBridge _fixture(Brightness brightness) {
  final bridge = WaddleBridge(
    events: const Stream<dynamic>.empty(),
    commandInvoker: (method, arguments) async => null,
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
    'timeline': 'chat',
    'wallpaperColors': false,
  });
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
    'newestFirst': false,
    'canConfigure': true,
    'typing': <String>[],
    'messages': [
      {
        'id': 'one',
        'body': 'Morning everyone! Any plans for the weekend?',
        'author': 'Alex',
        'authorJid': 'alex@example.org',
        'timestamp': '2026-10-31T10:00:00Z',
        'startsGroup': true,
        'mine': false,
      },
      {
        'id': 'two',
        'body': 'Thinking about a walk by the river if the weather holds.',
        'author': 'David',
        'authorJid': 'david@example.org',
        'timestamp': '2026-10-31T10:02:00Z',
        'startsGroup': true,
        'mine': true,
      },
      {
        'id': 'three',
        'body': 'That sounds lovely. Count me in!',
        'author': 'Sam',
        'authorJid': 'sam@example.org',
        'timestamp': '2026-10-31T10:03:00Z',
        'startsGroup': true,
        'mine': false,
        'replyCount': 2,
        'threadTargetId': 'three-thread',
      },
      {
        'id': 'four',
        'body': 'I’ll bring coffee. Any special requests?',
        'author': 'Alex',
        'authorJid': 'alex@example.org',
        'timestamp': '2026-10-31T10:05:00Z',
        'startsGroup': true,
        'mine': false,
      },
      {
        'id': 'five',
        'body': 'Perfect. Let’s meet at the bridge around 10.',
        'author': 'David',
        'authorJid': 'david@example.org',
        'timestamp': '2026-10-31T10:07:00Z',
        'startsGroup': true,
        'mine': true,
      },
    ],
  });
  return bridge;
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
  await tester.runAsync(() async {
    final boundary =
        boundaryKey.currentContext!.findRenderObject()!
            as RenderRepaintBoundary;
    final image = await boundary.toImage(pixelRatio: 2);
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
    'settings',
    'profile',
    'call',
    'login',
  ]) {
    testWidgets('$screen fits 320px with double-size text', (tester) async {
      await _render(tester, screen, Brightness.light, const Size(320, 740), 2);
    });
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
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);
  final previousShadows = debugDisableShadows;
  debugDisableShadows = false;
  final bridge = _fixture(brightness);
  final ownedByApp =
      screen == 'home' || screen == 'conversation' || screen == 'login';
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
          data: MediaQueryData(
            size: size,
            textScaler: TextScaler.linear(scale),
          ),
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
    if (screen == 'conversation') {
      bridge.acceptEvent({
        'topic': 'navigate',
        'data': {'jid': _room, 'title': 'General', 'group': true},
      });
      await tester.pumpAndSettle();
    }
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

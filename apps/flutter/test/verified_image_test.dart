import 'dart:async';
import 'dart:convert';
import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_flutter/bridge.dart';
import 'package:waddle_flutter/features/conversation.dart';
import 'package:waddle_flutter/features/verified_image.dart';

final _png = base64Decode(
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==',
);

void main() {
  test(
    'cache evicts by bytes, retries failures and fences cleared requests',
    () async {
      final cache = VerifiedImageCache(maxBytes: 4);
      var calls = 0;
      Future<Uint8List> fetch(int value) async {
        calls++;
        return Uint8List.fromList([value, value]);
      }

      await cache.load('a', () => fetch(1));
      await cache.load('b', () => fetch(2));
      await cache.load('a', () => fetch(1));
      await cache.load('c', () => fetch(3));
      await cache.load('b', () => fetch(2));
      expect(calls, 4);

      await expectLater(
        cache.load('bad', () async => throw StateError('unavailable')),
        throwsStateError,
      );
      expect(await cache.load('bad', () => fetch(4)), [4, 4]);

      final old = Completer<Uint8List>();
      final oldResult = cache.load('account', () => old.future);
      cache.clear();
      old.complete(Uint8List.fromList([7]));
      await oldResult;
      expect(await cache.load('account', () => fetch(8)), [8, 8]);
    },
  );

  testWidgets('plain image previews use verified bytes and can retry', (
    tester,
  ) async {
    var requests = 0;
    final bridge = WaddleBridge(
      events: const Stream<dynamic>.empty(),
      commandInvoker: (method, args) async {
        if (method == 'conversation.fileBytes') {
          requests++;
          if (requests == 1) throw StateError('integrity check failed');
          return _png;
        }
        return null;
      },
    );
    addTearDown(bridge.dispose);
    bridge.acceptEvent({
      'generation': 1,
      'topic': 'connection',
      'data': {'state': 'ready'},
    });
    void timeline(String revision) => bridge.acceptEvent({
      'generation': 1,
      'topic': 'conversation',
      'data': {
        'jid': 'person@example.org',
        'loading': false,
        'reachedStart': true,
        'messages': [
          {
            'id': 'message',
            'author': 'Person',
            'startsGroup': true,
            'files': [
              {
                'id': '0',
                'name': 'photo.png',
                'mimeType': 'image/png',
                'url': 'https://files.example.org/photo.png',
                'encrypted': false,
                'contentRevision': revision,
              },
            ],
          },
        ],
      },
    });
    timeline('first');
    await tester.pumpWidget(
      MaterialApp(
        navigatorObservers: [waddleRoutes],
        home: ConversationScreen(
          bridge: bridge,
          jid: 'person@example.org',
          title: 'Person',
          group: false,
        ),
      ),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    expect(requests, 1);
    expect(bridge.error, isNull);
    expect(find.byTooltip('Retry image'), findsOneWidget);
    await tester.tap(find.byTooltip('Retry image'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 100));
    expect(requests, 2);
    expect(find.byType(VerifiedImage), findsOneWidget);
    final image = tester.widget<Image>(
      find.descendant(
        of: find.byType(VerifiedImage),
        matching: find.byType(Image),
      ),
    );
    expect(image.image, isA<MemoryImage>());

    timeline('updated');
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 100));
    expect(requests, 3);
    await tester.pumpWidget(const SizedBox.shrink());
  });

  testWidgets('undecodable bytes are evicted before retry', (tester) async {
    var requests = 0;
    final cache = VerifiedImageCache();
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: VerifiedImage(
            name: 'photo.png',
            cache: cache,
            cacheKey: 'photo',
            connected: true,
            fetch: () async =>
                ++requests == 1 ? Uint8List.fromList([1, 2, 3]) : _png,
            onOpen: () {},
          ),
        ),
      ),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 100));
    expect(find.byTooltip('Retry image'), findsOneWidget);
    await tester.tap(find.byTooltip('Retry image'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 100));
    expect(requests, 2);
    expect(find.byTooltip('Retry image'), findsNothing);
  });

  testWidgets('avatars wait for readiness and retry after reconnect', (
    tester,
  ) async {
    var requests = 0;
    final bridge = WaddleBridge(
      events: const Stream<dynamic>.empty(),
      commandInvoker: (method, args) async {
        if (method == 'profile.avatarBytes') {
          requests++;
          if (requests == 1) throw StateError('offline');
          return null;
        }
        return null;
      },
    );
    addTearDown(bridge.dispose);
    bridge.acceptEvent({
      'generation': 1,
      'topic': 'conversation',
      'data': {
        'jid': 'person@example.org',
        'loading': false,
        'reachedStart': true,
        'messages': [
          {
            'id': 'message',
            'author': 'Person',
            'authorJid': 'person@example.org',
            'body': 'Hello',
            'startsGroup': true,
          },
        ],
      },
    });
    void connection(String state) => bridge.acceptEvent({
      'generation': 1,
      'topic': 'connection',
      'data': {'state': state},
    });
    await tester.pumpWidget(
      MaterialApp(
        navigatorObservers: [waddleRoutes],
        home: ConversationScreen(
          bridge: bridge,
          jid: 'person@example.org',
          title: 'Person',
          group: false,
        ),
      ),
    );
    await tester.pump();
    expect(requests, 0);
    connection('ready');
    await tester.pump();
    await tester.pump();
    expect(requests, 1);
    expect(bridge.error, isNull);
    connection('reconnecting');
    await tester.pump();
    connection('ready');
    await tester.pump();
    await tester.pump();
    expect(requests, 2);
    await tester.pumpWidget(const SizedBox.shrink());
  });
}

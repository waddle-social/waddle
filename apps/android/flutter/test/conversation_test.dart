import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_android/bridge.dart';
import 'package:waddle_android/features/conversation.dart';

const _jid = 'general@rooms.example.org';

class _ConversationHarness {
  _ConversationHarness() {
    bridge = WaddleBridge(
      events: const Stream<dynamic>.empty(),
      commandInvoker: (method, arguments) async {
        commands.add((method, Map<String, dynamic>.from(arguments)));
        if (responses.containsKey(method)) {
          return responses[method];
        }
        if (method == 'conversation.mentionSearch') {
          return [
            {'jid': 'alex@example.org', 'name': 'alex'},
          ];
        }
        if (method == 'conversation.search') {
          return <dynamic>[];
        }
        return null;
      },
    );
  }

  late final WaddleBridge bridge;
  final commands = <(String, Map<String, dynamic>)>[];
  final responses = <String, dynamic>{};
  final navigator = GlobalKey<NavigatorState>();

  void snapshot({
    bool social = false,
    bool messages = false,
    bool manageRoom = false,
  }) {
    bridge.acceptEvent({
      'topic': 'preferences',
      'data': {'timeline': social ? 'social' : 'chat'},
    });
    bridge.acceptEvent({
      'topic': 'conversation',
      'data': {
        'jid': _jid,
        'loading': false,
        'reachedStart': true,
        'newestFirst': social,
        'canConfigure': manageRoom,
        'messages': messages
            ? [
                {
                  'id': 'old',
                  'body': 'Older message',
                  'author': 'Alex',
                  'timestamp': '2026-10-31T10:00:00Z',
                  'startsGroup': true,
                  'mine': false,
                  'replyCount': 1,
                  'threadTargetId': 'old-thread',
                },
                {
                  'id': 'new',
                  'body': 'Newer message',
                  'author': 'Blair',
                  'timestamp': '2026-10-31T10:05:00Z',
                  'startsGroup': true,
                  'mine': false,
                },
              ]
            : <dynamic>[],
      },
    });
  }

  Widget app() => MaterialApp(
    restorationScopeId: 'conversation-test',
    navigatorKey: navigator,
    navigatorObservers: [waddleRoutes],
    theme: ThemeData(useMaterial3: true),
    onGenerateRoute: (settings) {
      if (settings.name != 'conversation') {
        return null;
      }
      final args = settings.arguments! as Map;
      return MaterialPageRoute<void>(
        settings: settings,
        builder: (_) => ConversationScreen(
          bridge: bridge,
          jid: args['jid'] as String,
          title: args['title'] as String,
          group: args['group'] as bool,
          threadId: args['threadId'] as String?,
        ),
      );
    },
    home: ConversationScreen(
      bridge: bridge,
      jid: _jid,
      title: 'General',
      group: true,
    ),
  );

  List<Map<String, dynamic>> calls(String method) => [
    for (final command in commands)
      if (command.$1 == method) command.$2,
  ];
}

Future<void> _mount(WidgetTester tester, _ConversationHarness harness) async {
  tester.view.physicalSize = const Size(430, 1100);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);
  addTearDown(harness.bridge.dispose);
  await tester.pumpWidget(harness.app());
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 400));
}

Finder _body(String text) => find.byWidgetPredicate(
  (widget) => widget is SelectableText && widget.data == text,
);

void main() {
  testWidgets('chat and social timelines keep the correct live edge', (
    tester,
  ) async {
    final harness = _ConversationHarness()..snapshot(messages: true);
    await _mount(tester, harness);
    expect(
      tester.getTopLeft(_body('Older message')).dy,
      lessThan(tester.getTopLeft(_body('Newer message')).dy),
    );
    expect(tester.widget<ListView>(find.byType(ListView)).reverse, isTrue);

    harness.snapshot(social: true, messages: true);
    await tester.pump();
    await tester.pump();
    expect(
      tester.getTopLeft(_body('Newer message')).dy,
      lessThan(tester.getTopLeft(_body('Older message')).dy),
    );
    expect(tester.widget<ListView>(find.byType(ListView)).reverse, isFalse);
    expect(tester.takeException(), isNull);
    await tester.pumpWidget(const SizedBox.shrink());
  });

  testWidgets('selected mentions use Unicode code point send offsets', (
    tester,
  ) async {
    final harness = _ConversationHarness()..snapshot();
    await _mount(tester, harness);
    await tester.enterText(find.byType(TextField), '😀 hi @al');
    await tester.pump(const Duration(milliseconds: 300));
    await tester.pump();
    await tester.tap(find.text('alex'));
    await tester.pump();
    await tester.restartAndRestore();
    await tester.pump(const Duration(milliseconds: 400));
    expect(
      tester.widget<TextField>(find.byType(TextField)).controller!.text,
      '😀 hi @alex ',
    );
    await tester.tap(find.byTooltip('Send message'));
    await tester.pump();
    final send = harness.calls('conversation.send').single;
    expect(send['body'], '😀 hi @alex');
    expect(send['mentions'], [
      {'uri': 'xmpp:alex@example.org', 'begin': 5, 'end': 10},
    ]);
    expect(send['jid'], _jid);
    expect(send['group'], isTrue);
    expect(send['viewId'], isA<String>());
    await tester.pumpWidget(const SizedBox.shrink());
  });

  testWidgets('search overlays hide reading while retaining the native model', (
    tester,
  ) async {
    final harness = _ConversationHarness()..snapshot();
    await _mount(tester, harness);
    final viewId = harness.calls('conversation.open').single['viewId'];
    harness.commands.clear();

    await tester.tap(find.byTooltip('Search messages'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    expect(
      harness.calls('conversation.visibility'),
      contains(containsPair('visible', false)),
    );
    expect(harness.calls('conversation.close'), isEmpty);
    final searchInput = find.descendant(
      of: find.byType(SearchBar),
      matching: find.byType(TextField),
    );
    await tester.enterText(searchInput, 'hello');
    await tester.tap(find.byTooltip('Search'));
    await tester.pump();
    expect(
      harness.calls('conversation.search').single,
      containsPair('query', 'hello'),
    );

    harness.navigator.currentState!.pop();
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    expect(harness.calls('conversation.open').last['viewId'], viewId);
    expect(harness.calls('conversation.close'), isEmpty);
    await tester.pumpWidget(const SizedBox.shrink());
  });

  testWidgets(
    'old history restores its scroll position without reading the live edge',
    (tester) async {
      final harness = _ConversationHarness()..snapshot();
      harness.bridge.acceptEvent({
        'topic': 'conversation',
        'data': {
          'jid': _jid,
          'loading': false,
          'reachedStart': true,
          'messages': List.generate(
            80,
            (index) => {
              'id': 'message-$index',
              'body': 'History message $index',
              'author': 'Alex',
              'mine': false,
              'startsGroup': true,
              'timestamp': DateTime.utc(
                2026,
                10,
                31,
                8,
                index,
              ).toIso8601String(),
            },
          ),
        },
      });
      await _mount(tester, harness);
      await tester.drag(find.byType(ListView), const Offset(0, 500));
      await tester.pump(const Duration(milliseconds: 400));
      final offset = tester
          .widget<ListView>(find.byType(ListView))
          .controller!
          .offset;
      expect(offset, greaterThan(72));
      await tester.restartAndRestore();
      await tester.pump(const Duration(milliseconds: 400));
      expect(harness.calls('conversation.open').last['atNewestEdge'], isFalse);
      expect(
        tester.widget<ListView>(find.byType(ListView)).controller!.offset,
        closeTo(offset, 1),
      );
      await tester.pumpWidget(const SizedBox.shrink());
    },
  );

  testWidgets('room form controllers remain valid through dialog dismissal', (
    tester,
  ) async {
    final harness = _ConversationHarness()..snapshot(manageRoom: true);
    harness.responses['room.config'] = {
      'name': 'General',
      'description': '',
      'forum': false,
      'pinPermission': 'admins_only',
    };
    await _mount(tester, harness);
    await tester.tap(find.byTooltip('Conversation options'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Room settings'));
    await tester.pumpAndSettle();
    await tester.enterText(
      find.byWidgetPredicate(
        (widget) =>
            widget is TextField && widget.decoration?.labelText == 'Name',
      ),
      'Renamed',
    );
    await tester.tap(find.text('Save'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    expect(harness.calls('room.save').single['name'], 'Renamed');
    expect(tester.takeException(), isNull);
    await tester.pumpWidget(const SizedBox.shrink());
  });

  testWidgets('returning from a same-JID thread restores its parent owner', (
    tester,
  ) async {
    final harness = _ConversationHarness()..snapshot();
    await _mount(tester, harness);
    final parentId = harness.calls('conversation.open').single['viewId'];
    unawaited(
      harness.navigator.currentState!.push(
        MaterialPageRoute<void>(
          builder: (_) => ConversationScreen(
            bridge: harness.bridge,
            jid: _jid,
            title: 'General',
            group: true,
            threadId: 'root-message',
          ),
        ),
      ),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    final threadId = harness.calls('conversation.open').last['viewId'];
    expect(threadId, isNot(parentId));
    expect(harness.calls('conversation.open').last['threadId'], 'root-message');

    harness.navigator.currentState!.pop();
    await tester.pumpAndSettle();
    expect(harness.calls('conversation.open').last['viewId'], parentId);
    expect(
      harness.calls('conversation.close'),
      contains(containsPair('viewId', threadId)),
    );
    await tester.pumpWidget(const SizedBox.shrink());
  });

  testWidgets('system picker pause preserves the upload model and owner', (
    tester,
  ) async {
    final harness = _ConversationHarness()..snapshot();
    await _mount(tester, harness);
    final viewId = harness.calls('conversation.open').single['viewId'];
    await tester.tap(find.byTooltip('Attach encrypted file'));
    await tester.pump();
    expect(harness.calls('conversation.attach').single['viewId'], viewId);
    harness.commands.clear();

    for (final state in [
      AppLifecycleState.inactive,
      AppLifecycleState.hidden,
      AppLifecycleState.paused,
    ]) {
      tester.binding.handleAppLifecycleStateChanged(state);
    }
    await tester.pump();
    for (final state in [
      AppLifecycleState.hidden,
      AppLifecycleState.inactive,
      AppLifecycleState.resumed,
    ]) {
      tester.binding.handleAppLifecycleStateChanged(state);
    }
    await tester.pump();
    expect(harness.calls('conversation.open'), isEmpty);
    expect(harness.calls('conversation.close'), isEmpty);
    expect(
      harness.calls('conversation.visibility').map((args) => args['visible']),
      [false, true],
    );
    expect(
      harness
          .calls('conversation.visibility')
          .every((args) => args['viewId'] == viewId),
      isTrue,
    );
    await tester.pumpWidget(const SizedBox.shrink());
  });

  testWidgets('thread actions retain route arguments after restoration', (
    tester,
  ) async {
    final harness = _ConversationHarness()..snapshot(messages: true);
    await _mount(tester, harness);
    await tester.tap(find.text('1 reply'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    expect(harness.calls('conversation.open').last['threadId'], 'old-thread');
    await tester.restartAndRestore();
    await tester.pump(const Duration(milliseconds: 400));
    expect(harness.calls('conversation.open').last['threadId'], 'old-thread');
    expect(harness.calls('conversation.open').last['jid'], _jid);
    await tester.pumpWidget(const SizedBox.shrink());
  });

  testWidgets('keyboard images and image paste retain the native upload path', (
    tester,
  ) async {
    final harness = _ConversationHarness()..snapshot();
    await _mount(tester, harness);
    final field = tester.widget<TextField>(find.byType(TextField));
    expect(field.contentInsertionConfiguration!.allowedMimeTypes, ['image/*']);
    final bytes = Uint8List.fromList([1, 2, 3]);
    field.contentInsertionConfiguration!.onContentInserted(
      KeyboardInsertedContent(
        mimeType: 'image/png',
        uri: 'content://keyboard/image',
        data: bytes,
      ),
    );
    await tester.pump();
    expect(
      harness.calls('conversation.insertContent').single,
      containsPair('uri', 'content://keyboard/image'),
    );
    expect(
      harness.calls('conversation.insertContent').single,
      containsPair('jid', _jid),
    );
    expect(
      harness.calls('conversation.insertContent').single['data'],
      same(bytes),
    );
    expect(
      harness.calls('conversation.insertContent').single['mimeType'],
      'image/png',
    );
    await tester.tap(find.byTooltip('Paste image'));
    await tester.pump();
    expect(harness.calls('conversation.pasteImage'), hasLength(1));
    expect(harness.calls('conversation.send'), isEmpty);
    await tester.pumpWidget(const SizedBox.shrink());
  });
}

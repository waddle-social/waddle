import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_flutter/bridge.dart';
import 'package:waddle_flutter/features/conversation.dart';

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
  double textScale = 1;
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

  void timeline(List<Map<String, dynamic>> messages, {bool social = false}) {
    snapshot(social: social);
    bridge.acceptEvent({
      'topic': 'conversation',
      'data': {
        'jid': _jid,
        'loading': false,
        'reachedStart': true,
        'newestFirst': social,
        'messages': messages,
      },
    });
  }

  Widget app() => MaterialApp(
    restorationScopeId: 'conversation-test',
    navigatorKey: navigator,
    navigatorObservers: [waddleRoutes],
    theme: ThemeData(useMaterial3: true),
    builder: (context, child) => MediaQuery(
      data: MediaQuery.of(
        context,
      ).copyWith(textScaler: TextScaler.linear(textScale)),
      child: child!,
    ),
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

Map<String, dynamic> _message(
  String id,
  int minute, {
  String jid = 'alex@example.org',
  String author = 'Alex',
}) => {
  'id': id,
  'body': 'Message $id',
  'author': author,
  'authorJid': jid,
  'mine': false,
  'timestamp': DateTime.utc(2026, 10, 31, 10, minute).toIso8601String(),
  'startsGroup': true,
};

Finder _groupHeader(String id) => find.byKey(ValueKey('message-header:$id'));

void main() {
  testWidgets(
    'compact conversation keeps its tools reachable at double text size',
    (tester) async {
      final harness = _ConversationHarness()
        ..snapshot(messages: true)
        ..textScale = 2;
      await _mount(tester, harness);
      tester.view.physicalSize = const Size(320, 900);
      tester.view.viewInsets = const FakeViewPadding(bottom: 300);
      addTearDown(tester.view.resetViewInsets);
      await tester.pumpAndSettle();
      await tester.enterText(
        find.byType(TextField),
        'A message at larger size',
      );
      expect(find.byTooltip('Attach encrypted file'), findsOneWidget);
      expect(find.byTooltip('Send message'), findsOneWidget);
      await tester.tap(find.byTooltip('More message options'));
      await tester.pumpAndSettle();
      expect(find.text('Paste image'), findsOneWidget);
      expect(find.text('Slash commands'), findsOneWidget);
      await tester.ensureVisible(find.text('Paste image'));
      await tester.tap(find.text('Paste image'));
      await tester.pumpAndSettle();
      expect(harness.calls('conversation.pasteImage'), hasLength(1));
      await tester.tap(find.byTooltip('Send message'));
      await tester.pump();
      expect(
        harness.calls('conversation.send').single['body'],
        'A message at larger size',
      );
      expect(tester.takeException(), isNull);
      await tester.pumpWidget(const SizedBox.shrink());
    },
  );

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
    expect(
      tester.getTopLeft(find.byKey(const ValueKey('conversation-composer'))).dy,
      greaterThanOrEqualTo(tester.getBottomLeft(find.byType(ListView)).dy),
    );

    harness.snapshot(social: true, messages: true);
    await tester.pump();
    await tester.pump();
    expect(
      tester.getTopLeft(_body('Newer message')).dy,
      lessThan(tester.getTopLeft(_body('Older message')).dy),
    );
    expect(tester.widget<ListView>(find.byType(ListView)).reverse, isFalse);
    expect(
      tester.getTopLeft(find.byKey(const ValueKey('conversation-composer'))).dy,
      greaterThanOrEqualTo(tester.getBottomLeft(find.byType(ListView)).dy),
    );
    expect(tester.takeException(), isNull);
    await tester.pumpWidget(const SizedBox.shrink());
  });

  testWidgets(
    'message groups use sender identity and follow the visual order',
    (tester) async {
      final messages = [
        _message('one', 0, jid: 'one@example.org', author: 'Same name'),
        _message('two', 1, jid: 'two@example.org', author: 'Same name'),
        _message(
          'three',
          2,
          jid: 'two@example.org',
          author: 'New display name',
        ),
      ];
      final harness = _ConversationHarness()..timeline(messages);
      await _mount(tester, harness);
      expect(_groupHeader('one'), findsOneWidget);
      expect(_groupHeader('two'), findsOneWidget);
      expect(_groupHeader('three'), findsNothing);
      expect(tester.getSize(_groupHeader('one')).height, lessThan(32));
      expect(
        tester.getTopLeft(_body('Message one')).dy -
            tester.getBottomLeft(_groupHeader('one')).dy,
        lessThanOrEqualTo(2),
      );
      expect(
        tester.getSize(find.byTooltip('Message actions').first).height,
        greaterThanOrEqualTo(48),
      );
      final groupedTime =
          MaterialLocalizations.of(
            tester.element(_body('Message three')),
          ).formatTimeOfDay(
            TimeOfDay.fromDateTime(
              DateTime.parse(messages.last['timestamp'] as String).toLocal(),
            ),
          );
      expect(find.text(groupedTime), findsNothing);
      expect(find.byTooltip('Message actions · $groupedTime'), findsOneWidget);
      harness.timeline(messages, social: true);
      await tester.pumpAndSettle();
      expect(_groupHeader('three'), findsOneWidget);
      expect(_groupHeader('two'), findsNothing);
      expect(_groupHeader('one'), findsOneWidget);
      await tester.pumpWidget(const SizedBox.shrink());
    },
  );

  for (final social in [false, true]) {
    testWidgets(
      '${social ? 'social' : 'chat'} places the oldest-unread boundary between read and unread groups',
      (tester) async {
        final harness = _ConversationHarness()
          ..timeline([
            _message('read-A', 0),
            {..._message('oldest-unread-B', 1), 'unreadDivider': true},
            _message('new-unread-C', 2),
          ], social: social);
        await _mount(tester, harness);
        final divider = tester.getRect(
          find.byKey(const ValueKey('unread-divider:oldest-unread-B')),
        );
        if (social) {
          expect(_groupHeader('new-unread-C'), findsOneWidget);
          expect(_groupHeader('oldest-unread-B'), findsNothing);
          expect(_groupHeader('read-A'), findsOneWidget);
          expect(
            divider.top,
            greaterThanOrEqualTo(
              tester.getBottomLeft(_body('Message oldest-unread-B')).dy,
            ),
          );
          expect(
            divider.bottom,
            lessThanOrEqualTo(tester.getTopLeft(_groupHeader('read-A')).dy),
          );
        } else {
          expect(_groupHeader('read-A'), findsOneWidget);
          expect(_groupHeader('oldest-unread-B'), findsOneWidget);
          expect(_groupHeader('new-unread-C'), findsNothing);
          expect(
            divider.top,
            greaterThanOrEqualTo(
              tester.getBottomLeft(_body('Message read-A')).dy,
            ),
          );
          expect(
            divider.bottom,
            lessThanOrEqualTo(
              tester.getTopLeft(_groupHeader('oldest-unread-B')).dy,
            ),
          );
        }
        expect(tester.takeException(), isNull);
        await tester.pumpWidget(const SizedBox.shrink());
      },
    );
  }

  testWidgets('day unread system and call rows break message groups', (
    tester,
  ) async {
    final harness = _ConversationHarness()
      ..timeline([
        _message('first', 0),
        {..._message('unread', 1), 'unreadDivider': true},
        {..._message('system', 2), 'system': true},
        _message('after-system', 3),
        {..._message('call', 4), 'isCallRow': true},
        _message('after-call', 5),
        {..._message('next-day', 6), 'timestamp': '2026-11-01T10:06:00Z'},
      ]);
    await _mount(tester, harness);
    for (final id in [
      'first',
      'unread',
      'system',
      'after-system',
      'call',
      'after-call',
      'next-day',
    ]) {
      expect(
        _groupHeader(id),
        findsOneWidget,
        reason: '$id starts a visible group',
      );
    }
    expect(find.text('New messages'), findsOneWidget);
    await tester.pumpWidget(const SizedBox.shrink());
  });

  for (final state in ['pending', 'deleted']) {
    testWidgets(
      '$state message safety details are explicit and absent from the feed',
      (tester) async {
        final harness = _ConversationHarness()
          ..timeline([
            {
              ..._message('scored', 0),
              state: true,
              'safetyScores': {
                'modelVersion': 'safety-v1',
                'scores': [
                  {'category': 'toxicity', 'probability': .2},
                ],
              },
            },
          ]);
        await _mount(tester, harness);
        expect(find.text('Safety scores'), findsNothing);
        await tester.tap(find.byTooltip('Message actions'));
        await tester.pumpAndSettle();
        expect(find.text('Safety scores'), findsOneWidget);
        expect(find.text('Edit'), findsNothing);
        expect(find.text('Reply'), findsNothing);
        await tester.tap(find.text('Safety scores'));
        await tester.pumpAndSettle();
        expect(find.text('Model: safety-v1'), findsOneWidget);
        harness.navigator.currentState!.pop();
        await tester.pumpAndSettle();
        await tester.pumpWidget(const SizedBox.shrink());
      },
    );
  }

  testWidgets(
    'reply context and long editor fit a small viewport above the keyboard',
    (tester) async {
      final harness = _ConversationHarness()
        ..timeline([_message('reply-target', 0)])
        ..textScale = 2;
      await _mount(tester, harness);
      await tester.tap(find.byTooltip('Message actions'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('Reply'));
      await tester.pumpAndSettle();
      tester.view.physicalSize = const Size(320, 740);
      tester.view.viewInsets = const FakeViewPadding(bottom: 300);
      addTearDown(tester.view.resetViewInsets);
      await tester.pumpAndSettle();
      const draft =
          'A longer draft with several lines of text that must scroll inside the editor while reply controls stay reachable.';
      await tester.enterText(find.byType(TextField), draft);
      await tester.pumpAndSettle();
      final composer = tester.getRect(
        find.byKey(const ValueKey('conversation-composer')),
      );
      expect(composer.bottom, lessThanOrEqualTo(440));
      expect(composer.height, lessThan(280));
      final editorClip = find.byKey(const ValueKey('composer-editor-clip'));
      expect(tester.widget<ClipRect>(editorClip).clipBehavior, Clip.hardEdge);
      expect(
        tester.getTopLeft(editorClip).dy,
        greaterThanOrEqualTo(
          tester.getBottomLeft(find.byTooltip('Cancel reply')).dy,
        ),
      );
      expect(
        find.ancestor(of: find.byType(TextField), matching: editorClip),
        findsOneWidget,
      );
      expect(
        tester.getSize(find.byTooltip('Cancel reply')).height,
        greaterThanOrEqualTo(48),
      );
      expect(
        tester.getSize(find.byTooltip('Send message')).height,
        greaterThanOrEqualTo(48),
      );
      await tester.tap(find.byTooltip('Cancel reply'));
      await tester.pumpAndSettle();
      expect(
        tester.widget<TextField>(find.byType(TextField)).controller!.text,
        draft,
      );
      expect(tester.takeException(), isNull);
      await tester.pumpWidget(const SizedBox.shrink());
    },
  );

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

  testWidgets(
    'image paste keeps the selected reply until durable success and explicitly clears the next send',
    (tester) async {
      final harness = _ConversationHarness()
        ..timeline([_message('reply-image', 0)]);
      final paste = Completer<dynamic>();
      harness.responses['conversation.pasteImage'] = paste.future;
      await _mount(tester, harness);
      await tester.tap(find.byTooltip('Message actions'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('Reply'));
      await tester.pumpAndSettle();
      await tester.enterText(find.byType(TextField), 'Keep this text draft');
      await tester.tap(find.byTooltip('More message options'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('Paste image'));
      await tester.pump(const Duration(milliseconds: 400));
      expect(
        harness.calls('conversation.pasteImage').single['replyTo'],
        'reply-image',
      );
      expect(find.byTooltip('Cancel reply'), findsOneWidget);
      paste.completeError(StateError('Upload failed before enqueue'));
      await tester.pump(const Duration(milliseconds: 400));
      expect(find.byTooltip('Cancel reply'), findsOneWidget);
      expect(
        tester.widget<TextField>(find.byType(TextField)).controller!.text,
        'Keep this text draft',
      );
      harness.responses['conversation.pasteImage'] = true;
      await tester.tap(find.byTooltip('More message options'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('Paste image'));
      await tester.pump(const Duration(milliseconds: 400));
      expect(
        harness.calls('conversation.pasteImage').last['replyTo'],
        'reply-image',
      );
      expect(find.byTooltip('Cancel reply'), findsNothing);
      expect(
        tester.widget<TextField>(find.byType(TextField)).controller!.text,
        'Keep this text draft',
      );
      await tester.tap(find.byTooltip('More message options'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('Paste image'));
      await tester.pump(const Duration(milliseconds: 400));
      expect(
        harness.calls('conversation.pasteImage').last.containsKey('replyTo'),
        isTrue,
      );
      expect(harness.calls('conversation.pasteImage').last['replyTo'], isNull);
      expect(tester.takeException(), isNull);
      await tester.pumpWidget(const SizedBox.shrink());
    },
  );

  testWidgets('editing blocks immediate image send and keeps the edit draft', (
    tester,
  ) async {
    final harness = _ConversationHarness()
      ..timeline([
        {..._message('editable', 0), 'mine': true},
      ]);
    await _mount(tester, harness);
    await tester.tap(find.byTooltip('Message actions'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Edit'));
    await tester.pumpAndSettle();
    await tester.tap(find.byTooltip('More message options'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Paste image'));
    await tester.pumpAndSettle();
    expect(harness.calls('conversation.pasteImage'), isEmpty);
    expect(
      find.text(
        'Finish or cancel your edit before sending a file, GIF, or sticker.',
      ),
      findsOneWidget,
    );
    expect(
      tester.widget<TextField>(find.byType(TextField)).controller!.text,
      'Message editable',
    );
    expect(find.byTooltip('Save edit'), findsOneWidget);
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
    await tester.tap(find.byTooltip('More message options'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Paste image'));
    await tester.pump();
    expect(harness.calls('conversation.pasteImage'), hasLength(1));
    expect(harness.calls('conversation.send'), isEmpty);
    await tester.pumpWidget(const SizedBox.shrink());
  });
}

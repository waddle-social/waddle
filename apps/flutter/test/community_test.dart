import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_flutter/bridge.dart';
import 'package:waddle_flutter/features/community.dart';
import 'package:waddle_flutter/runtime/surfaces_runtime.dart';

import 'platform_test_context.dart';

void main() {
  test(
    'expired stories are hidden and read state comes from the account',
    () async {
      final context = TestRuntimeContext();
      context.handler = (method, args) async => switch (method) {
        'stories_items' => [
          {'id': 'old', 'expires': '2000-01-01T00:00:00Z'},
          {'id': 'active', 'expires': '2100-01-01T00:00:00Z'},
        ],
        'story_reads_fetch' => {
          'entries': [
            {'id': 'active'},
          ],
        },
        _ => throw StateError(method),
      };
      final runtime = SurfaceRuntime(context);
      addTearDown(runtime.dispose);
      final stories = await runtime.invoke('community.stories', {}) as List;
      expect(stories.single, containsPair('read', true));
      expect(stories.single, containsPair('id', 'active'));
      expect(
        context.calls.first['args']['community_jid'],
        'community.example.test',
      );
    },
  );

  test('concurrent story reads merge into the existing PEP state', () async {
    final context = TestRuntimeContext();
    var entries = <dynamic>[];
    context.handler = (method, args) async {
      if (method == 'story_reads_fetch') return {'entries': List.of(entries)};
      expect(method, 'story_reads_publish');
      entries = args['input']['entries'] as List;
      return {'entries': entries};
    };
    final runtime = SurfaceRuntime(context);
    addTearDown(runtime.dispose);
    await Future.wait([
      runtime.invoke('community.readStory', {'id': 'a'}),
      runtime.invoke('community.readStory', {'id': 'b'}),
    ]);
    expect(entries.map((row) => row['id']), ['a', 'b']);
  });

  test('event RSVPs are attached to their master event and account', () async {
    final context = TestRuntimeContext();
    context.handler = (_, _) async => [
      {'uid': 'one', 'id': 'one', 'summary': 'Meetup'},
      {
        'uid': 'one',
        'id': 'one-rsvp-alice',
        'attendees': [
          {'uri': 'xmpp:alice@example.test', 'partstat': 'ACCEPTED'},
        ],
      },
    ];
    final runtime = SurfaceRuntime(context);
    addTearDown(runtime.dispose);
    final events = await runtime.invoke('community.events', {}) as List;
    expect(events.single['responses'], {'alice@example.test': 'ACCEPTED'});
    await runtime.invoke('community.rsvp', {
      'id': 'one',
      'response': 'DECLINED',
    });
    expect(context.calls.last['args'], {
      'community_jid': 'community.example.test',
      'master_uid': 'one',
      'self_localpart': 'alice',
      'self_jid': 'alice@example.test',
      'partstat': 'DECLINED',
    });
  });

  testWidgets('submitting a new post reloads the feed', (tester) async {
    final input = <Map<String, dynamic>>[];
    final bridge = WaddleBridge(
      events: const Stream.empty(),
      commandInvoker: (method, args) async {
        if (method == 'community.post') {
          input.add(args);
          return true;
        }
        expect(method, 'community.feed');
        return [
          for (final post in input) {...post, 'author': 'Alice'},
        ];
      },
    );
    addTearDown(bridge.dispose);
    await tester.pumpWidget(
      MaterialApp(
        home: CommunityScreen(bridge: bridge, section: CommunitySection.feed),
      ),
    );
    await tester.pumpAndSettle();
    await tester.tap(find.byTooltip('New post'));
    await tester.pumpAndSettle();
    await tester.enterText(find.byType(TextField), 'Meet at the dock');
    await tester.pump();
    await tester.tap(find.text('Publish'));
    await tester.pumpAndSettle();
    expect(find.text('Meet at the dock'), findsOneWidget);
    expect(input, [
      {'body': 'Meet at the dock'},
    ]);
  });
}

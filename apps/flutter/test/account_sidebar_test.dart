import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_flutter/app.dart';
import 'package:waddle_flutter/bridge.dart';
import 'package:waddle_flutter/design.dart';
import 'package:waddle_flutter/features/community.dart';
import 'package:waddle_flutter/features/conversation.dart';
import 'package:waddle_flutter/features/remote_communities.dart';
import 'package:waddle_flutter/features/trip_plan.dart';

void main() {
  testWidgets(
    'roster photos coalesce and a previous account cannot fill a new avatar',
    (tester) async {
      final reads = <Completer<Uint8List?>>[];
      Future<Uint8List?> photo(String jid) async {
        if (jid != 'sasha@example.org') return null;
        final pending = Completer<Uint8List?>();
        reads.add(pending);
        return pending.future;
      }

      final bridge = await _mount(tester, width: 1440, photo: photo);
      expect(reads, hasLength(1));
      final home = Map.of(bridge.home);
      bridge.acceptEvent({
        'generation': 2,
        'topic': 'session',
        'data': {'state': 'ready', 'jid': 'second@example.org'},
      });
      bridge.acceptEvent({'generation': 2, 'topic': 'home', 'data': home});
      await tester.pumpAndSettle();
      expect(reads, hasLength(2));
      final png = base64Decode(
        'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/bIkAAAAASUVORK5CYII=',
      );
      reads.first.complete(png);
      await tester.pump();
      Iterable<WaddleAvatar> avatars() => tester
          .widgetList<WaddleAvatar>(find.byType(WaddleAvatar))
          .where((avatar) => avatar.name == 'Sasha');
      expect(avatars().length, greaterThan(1));
      expect(avatars().every((avatar) => avatar.bytes == null), isTrue);
      reads.last.complete(png);
      await tester.pump();
      await tester.pump();
      expect(avatars().every((avatar) => identical(avatar.bytes, png)), isTrue);
      await tester.pumpWidget(const SizedBox.shrink());
      bridge.dispose();
    },
  );
  testWidgets('community menu opens every account feature from a room', (
    tester,
  ) async {
    final bridge = await _mount(tester);
    await tester.tap(find.text('General').first);
    await tester.pumpAndSettle();
    expect(find.text('Room community'), findsOneWidget);
    expect(
      tester.widget<ConversationScreen>(find.byType(ConversationScreen)).jid,
      'general@rooms.example.org',
    );
    for (final name in ['Feed', 'Stories', 'Trips', 'Remote communities']) {
      await tester.tap(find.byTooltip('Community menu'));
      await tester.pumpAndSettle();
      await tester.tap(find.text(name).last);
      await tester.pumpAndSettle();
      switch (name) {
        case 'Feed':
          expect(
            tester
                .widget<CommunityScreen>(find.byType(CommunityScreen))
                .section,
            CommunitySection.feed,
          );
        case 'Stories':
          expect(
            tester
                .widget<CommunityScreen>(find.byType(CommunityScreen))
                .section,
            CommunitySection.stories,
          );
        case 'Trips':
          expect(find.byType(TripPlansScreen), findsOneWidget);
        case 'Remote communities':
          expect(find.byType(RemoteCommunitiesScreen), findsOneWidget);
      }
      expect(find.byTooltip('Browse all rooms'), findsOneWidget);
      expect(tester.takeException(), isNull);
    }
    await tester.tap(find.text('Calendar'));
    await tester.pumpAndSettle();
    expect(
      tester.widget<CommunityScreen>(find.byType(CommunityScreen)).section,
      CommunitySection.events,
    );
    await tester.pumpWidget(const SizedBox.shrink());
    bridge.dispose();
  });
  testWidgets('quick jump from the keyboard targets a group direct message', (
    tester,
  ) async {
    final bridge = await _mount(tester);
    await tester.sendKeyDownEvent(LogicalKeyboardKey.controlLeft);
    await tester.sendKeyEvent(LogicalKeyboardKey.keyK);
    await tester.sendKeyUpEvent(LogicalKeyboardKey.controlLeft);
    await tester.pumpAndSettle();
    final search = find.widgetWithText(
      TextField,
      'Search rooms, people or pages',
    );
    await tester.enterText(search, 'crew@rooms.example.org');
    await tester.pump();
    await tester.tap(
      find.descendant(of: find.byType(Dialog), matching: find.text('Crew')),
    );
    await tester.pumpAndSettle();
    final conversation = tester.widget<ConversationScreen>(
      find.byType(ConversationScreen),
    );
    expect(conversation.jid, 'crew@rooms.example.org');
    expect(conversation.group, isTrue);
    await tester.pumpWidget(const SizedBox.shrink());
    bridge.dispose();
  });

  testWidgets('dashboard counts real contacts once and shows the next event', (
    tester,
  ) async {
    final bridge = await _mount(tester, width: 1440);
    expect(find.text('Fall workshop'), findsOneWidget);
    expect(find.text('A past event'), findsNothing);
    expect(find.text('2 unread messages · 1 friend online'), findsOneWidget);
    expect(find.text('Jump into General'), findsOneWidget);
    await tester.tap(find.text('See events'));
    await tester.pumpAndSettle();
    expect(
      tester.widget<CommunityScreen>(find.byType(CommunityScreen)).section,
      CommunitySection.events,
    );
    await tester.pumpWidget(const SizedBox.shrink());
    bridge.dispose();
  });
}

Future<WaddleBridge> _mount(
  WidgetTester tester, {
  double width = 1000,
  Future<Uint8List?> Function(String jid)? photo,
}) async {
  tester.view.devicePixelRatio = 1;
  tester.view.physicalSize = Size(width, 850);
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);
  final now = DateTime.now();
  final bridge = WaddleBridge(
    events: const Stream.empty(),
    commandInvoker: (method, args) async {
      if (method == 'profile.avatarBytes') {
        return photo?.call(args['jid'] as String);
      }
      if (method == 'community.events') {
        return [
          for (final (id, days, name) in [
            ('past', -3, 'A past event'),
            ('upcoming', 3, 'Fall workshop'),
          ])
            {
              'id': id,
              'uid': id,
              'summary': name,
              'dtstart': {
                'kind': 'date-time',
                'ms': now.add(Duration(days: days)).millisecondsSinceEpoch,
              },
            },
        ];
      }
      return <dynamic>[];
    },
  );
  bridge.acceptEvent({
    'topic': 'session',
    'data': {'state': 'ready', 'jid': 'me@example.org'},
  });
  bridge.acceptEvent({
    'topic': 'home',
    'data': {
      'settled': true,
      'communityName': 'My community',
      'rooms': [
        {
          'name': 'General',
          'jid': 'general@rooms.example.org',
          'unread': 2,
          'communityName': 'Room community',
        },
        {'name': 'Crew', 'jid': 'crew@rooms.example.org', 'groupDm': true},
      ],
      'dmPeers': [
        {'jid': 'sasha@example.org', 'name': 'Sasha', 'show': 'dnd'},
      ],
      'people': [
        {'jid': 'sasha@example.org', 'name': 'Sasha', 'show': 'dnd'},
      ],
    },
  });
  await tester.pumpWidget(MaterialApp(home: WaddleShell(bridge: bridge)));
  await tester.pumpAndSettle();
  return bridge;
}

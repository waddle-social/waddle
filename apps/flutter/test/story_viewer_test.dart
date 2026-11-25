import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_flutter/bridge.dart';
import 'package:waddle_flutter/features/story_viewer.dart';

void main() {
  testWidgets(
    'stories skip expired entries and only mark displayed media as read',
    (tester) async {
      final calls = <String>[];
      final bridge = WaddleBridge(
        events: const Stream.empty(),
        commandInvoker: (method, args) async => calls.add(method),
      );
      addTearDown(bridge.dispose);
      const stories = [
        {
          'id': 'a',
          'author': 'Alice',
          'body': 'First',
          'media_url': 'http://example.test/a.png',
        },
        {
          'id': 'b',
          'author': 'Bob',
          'body': 'Expired',
          'expires': '2000-01-01T00:00:00Z',
        },
        {
          'id': 'c',
          'author': 'Carol',
          'body': 'Last',
          'media_url': 'https://user@example.test/c.png',
        },
      ];
      await tester.pumpWidget(
        MaterialApp(
          home: Builder(
            builder: (context) => TextButton(
              onPressed: () => Navigator.of(context).push(
                MaterialPageRoute<void>(
                  builder: (_) => StoryViewer(
                    bridge: bridge,
                    stories: stories,
                    initialIndex: 0,
                  ),
                ),
              ),
              child: const Text('Open stories'),
            ),
          ),
        ),
      );
      await tester.tap(find.text('Open stories'));
      await tester.pumpAndSettle();
      expect(find.text('First'), findsOneWidget);
      expect(find.text('This story has no usable media link.'), findsOneWidget);
      await tester.sendKeyEvent(LogicalKeyboardKey.arrowRight);
      await tester.pumpAndSettle();
      expect(find.text('Last'), findsOneWidget);
      await tester.tap(find.byTooltip('Previous story'));
      await tester.pumpAndSettle();
      expect(find.text('First'), findsOneWidget);
      await tester.tap(find.byTooltip('Next story'));
      await tester.pumpAndSettle();
      await tester.tap(find.byTooltip('Next story'));
      await tester.pumpAndSettle();
      expect(find.text('Open stories'), findsOneWidget);
      expect(calls, isEmpty);
    },
  );
}

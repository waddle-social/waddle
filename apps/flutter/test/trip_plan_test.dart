import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_flutter/bridge.dart';
import 'package:waddle_flutter/features/trip_plan.dart';

void main() {
  testWidgets('voting writes the selected option to the displayed room', (
    tester,
  ) async {
    final writes = <Map<String, dynamic>>[];
    final bridge = WaddleBridge(
      events: const Stream.empty(),
      commandInvoker: (method, args) async {
        if (method == 'planning.save') {
          writes.add(args);
          return args['item'];
        }
        expect(method, 'planning.list');
        return [
          {
            'kind': 'poll',
            'id': 'lunch',
            'title': 'Where for lunch?',
            'options': [
              {'id': 'one', 'label': 'Picnic'},
              {'id': 'two', 'label': 'Cafe'},
            ],
          },
          for (final write in writes)
            {...write['item'] as Map, 'author': 'alice@example.org'},
        ];
      },
    );
    addTearDown(bridge.dispose);
    bridge.acceptEvent({
      'topic': 'session',
      'data': {'state': 'ready', 'jid': 'alice@example.org'},
    });
    await tester.pumpWidget(
      MaterialApp(
        home: TripPlanScreen(bridge: bridge, room: 'weekend@rooms.example.org'),
      ),
    );
    await tester.pumpAndSettle();
    await tester.tap(find.text('Where for lunch?'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Picnic'));
    await tester.pumpAndSettle();
    expect(writes.single, {
      'room': 'weekend@rooms.example.org',
      'item': {
        'kind': 'vote',
        'title': '',
        'id': 'vote:lunch:alice@example.org',
        'subject': 'lunch',
        'selections': ['one'],
      },
    });
    expect(find.text('1 vote'), findsOneWidget);
  });
}

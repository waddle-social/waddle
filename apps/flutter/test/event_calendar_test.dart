import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_flutter/bridge.dart';
import 'package:waddle_flutter/features/calendar_occurrences.dart';
import 'package:waddle_flutter/features/community.dart';

void main() {
  testWidgets(
    'the attendance filter uses this account’s RSVP and offers the series export',
    (tester) async {
      final date = calendarDayString(DateTime.now());
      final bridge =
          WaddleBridge(
            events: const Stream.empty(),
            commandInvoker: (_, _) async => [
              for (final item in {
                'Picnic': 'ACCEPTED',
                'Concert': 'TENTATIVE',
                'Meeting': 'DECLINED',
              }.entries)
                {
                  'id': item.key,
                  'uid': item.key,
                  'summary': item.key,
                  'dtstart': {'kind': 'date', 'date': date},
                  'icalendar': 'BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n',
                  'responses': {'alice@example.test': item.value},
                },
            ],
          )..acceptEvent({
            'topic': 'session',
            'data': {'jid': 'alice@example.test'},
          });
      addTearDown(bridge.dispose);
      await tester.pumpWidget(
        MaterialApp(
          home: CommunityScreen(
            bridge: bridge,
            section: CommunitySection.events,
          ),
        ),
      );
      await tester.pumpAndSettle();
      await tester.ensureVisible(find.text('Going or maybe'));
      await tester.tap(find.text('Going or maybe'));
      await tester.pumpAndSettle();
      expect(find.text('Meeting'), findsNothing);
      await tester.ensureVisible(find.text('Concert'));
      await tester.tap(find.text('Concert'));
      await tester.pumpAndSettle();
      expect(find.text('Export event (.ics)'), findsOneWidget);
    },
  );
  testWidgets(
    'editing an occurrence sends its original date and just the changed fields',
    (tester) async {
      final date = calendarDayString(DateTime.now());
      final edits = <Map<String, dynamic>>[];
      final bridge = WaddleBridge(
        events: const Stream.empty(),
        commandInvoker: (method, args) async {
          switch (method) {
            case 'community.events':
              return [
                {
                  'id': 'one',
                  'uid': 'one',
                  'summary': 'Meet',
                  'organizer': 'xmpp:alice@example.test',
                  'dtstart': {'kind': 'date', 'date': date},
                  'rrule': {'freq': 'DAILY', 'count': 1},
                },
              ];
            case 'community.editEvent':
              edits.add(args);
              return null;
            default:
              throw StateError(method);
          }
        },
      );
      bridge.acceptEvent({
        'topic': 'session',
        'generation': 1,
        'data': {'jid': 'alice@example.test'},
      });
      addTearDown(bridge.dispose);
      await tester.pumpWidget(
        MaterialApp(
          home: CommunityScreen(
            bridge: bridge,
            section: CommunitySection.events,
          ),
        ),
      );
      await tester.pumpAndSettle();
      expect(find.text('Month agenda'), findsOneWidget);
      await tester.ensureVisible(find.text('Meet'));
      await tester.tap(find.text('Meet'));
      await tester.pumpAndSettle();
      await tester.drag(find.byType(ListView), const Offset(0, -450));
      await tester.pumpAndSettle();
      await tester.ensureVisible(find.text('Edit occurrence'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('Edit occurrence'));
      await tester.pumpAndSettle();
      await tester.enterText(find.widgetWithText(TextField, 'Meet'), 'Picnic');
      await tester.tap(find.text('Save event'));
      await tester.pumpAndSettle();
      expect(edits.single['occurrence'], {'kind': 'date', 'date': date});
      expect(edits.single['fields'], ['summary']);
      expect(edits.single['event']['summary'], 'Picnic');
    },
  );
}

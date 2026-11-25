import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_flutter/features/calendar_occurrences.dart';

Map<String, dynamic> day(String date) => {'kind': 'date', 'date': date};
EventData series(String start, Map<String, dynamic> rule) => {
  'id': 'meet',
  'uid': 'meet',
  'dtstart': day(start),
  'summary': 'Meet',
  'rrule': rule,
};
void main() {
  test(
    'weekly agenda applies count, EXDATE and a moved override using the original recurrence ID',
    () {
      final event = {
        ...series('2026-11-02', {
          'freq': 'WEEKLY',
          'by_day': ['MO', 'WE'],
          'count': 5,
        }),
        'dtend': day('2026-11-03'),
        'exdates': [day('2026-11-04')],
        'overrides': [
          {
            'recurrence_id': day('2026-11-09'),
            'dtstart': day('2026-11-12'),
            'summary': 'Moved',
          },
        ],
      };
      final result = calendarOccurrences(
        [event],
        DateTime(2026, 11),
        DateTime(2026, 12),
      );
      expect(result.map((r) => r['dtstart']['date']), [
        '2026-11-02',
        '2026-11-11',
        '2026-11-12',
        '2026-11-16',
      ]);
      expect(result[2]['summary'], 'Moved');
      expect(result[2]['instance'], day('2026-11-09'));
      expect(result[2]['dtend'], day('2026-11-13'));
    },
  );
  test(
    'monthly agenda skips missing dates, observes interval and includes spanning events on the selected day',
    () {
      final rows = calendarOccurrences(
        [
          series('2026-01-31', {'freq': 'MONTHLY', 'interval': 2, 'count': 3}),
        ],
        DateTime(2026, 2),
        DateTime(2026, 6),
      );
      expect(rows.map((r) => r['dtstart']['date']), [
        '2026-03-31',
        '2026-05-31',
      ]);
      final ongoing = {
        'id': 'trip',
        'dtstart': day('2026-03-29'),
        'dtend': day('2026-04-02'),
      };
      expect(
        calendarOccurrences(
          [ongoing],
          DateTime(2026, 4, 1),
          DateTime(2026, 4, 2),
        ),
        hasLength(1),
      );
      expect(
        calendarOccurrences(
          [ongoing],
          DateTime(2026, 4, 2),
          DateTime(2026, 4, 3),
        ),
        isEmpty,
      );
    },
  );
  test(
    'an old unbounded series is visible in the current month and totals honor UNTIL',
    () {
      final master = series('1995-12-01', {
        'freq': 'DAILY',
        'until': '2026-11-02T23:59:00Z',
      });
      final rows = calendarOccurrences(
        [master],
        DateTime(2026, 11),
        DateTime(2026, 12),
      );
      expect(rows.map((r) => r['dtstart']['date']), [
        '2026-11-01',
        '2026-11-02',
      ]);
    },
  );
}

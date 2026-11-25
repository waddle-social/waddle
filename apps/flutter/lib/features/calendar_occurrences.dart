/// xCal DATEs are floating calendar days. DATE-TIMEs are UTC instants.
DateTime? calendarValueTime(dynamic value) {
  if (value is! Map) return null;
  if (value['kind'] == 'date' && value['date'] is String) {
    return DateTime.tryParse('${value['date']}T00:00:00Z');
  }
  final ms = value['ms'];
  return ms is num
      ? DateTime.fromMillisecondsSinceEpoch(ms.round(), isUtc: true)
      : null;
}

String calendarDayString(DateTime date) =>
    '${date.year.toString().padLeft(4, '0')}-${date.month.toString().padLeft(2, '0')}-${date.day.toString().padLeft(2, '0')}';

Map<String, dynamic> calendarValue(DateTime time, bool allDay) => allDay
    ? {'kind': 'date', 'date': calendarDayString(time)}
    : {'kind': 'date-time', 'ms': time.millisecondsSinceEpoch};

DateTime? calendarDisplayTime(dynamic value) {
  final time = calendarValueTime(value);
  if (time == null) return null;
  return value['kind'] == 'date'
      ? DateTime(time.year, time.month, time.day)
      : time.toLocal();
}

typedef EventData = Map<String, dynamic>;

const _day = Duration(days: 1);
const _weekdays = ['MO', 'TU', 'WE', 'TH', 'FR', 'SA', 'SU'];

bool _matches(DateTime cursor, DateTime start, Map rule) {
  final rawInterval = rule['interval'];
  final n = rawInterval is int && rawInterval > 0 ? rawInterval : 1;
  final days = cursor.difference(start).inDays;
  final months = (cursor.year - start.year) * 12 + cursor.month - start.month;
  final byDay = (rule['by_day'] as List?) ?? const [];
  final byMonthDay = (rule['by_month_day'] as List?) ?? const [];
  if (byDay.isNotEmpty && !byDay.contains(_weekdays[cursor.weekday - 1])) {
    return false;
  }
  if (byMonthDay.isNotEmpty) {
    final length = DateTime.utc(cursor.year, cursor.month + 1, 0).day;
    if (!byMonthDay.contains(cursor.day) &&
        !byMonthDay.contains(cursor.day - length - 1)) {
      return false;
    }
  }
  return switch (rule['freq']) {
    'DAILY' => days % n == 0,
    'WEEKLY' =>
      ((days + start.weekday - 1) ~/ 7) % n == 0 &&
          (byDay.isNotEmpty || cursor.weekday == start.weekday),
    'MONTHLY' =>
      months % n == 0 &&
          (byMonthDay.isNotEmpty ||
              byDay.isNotEmpty ||
              cursor.day == start.day),
    'YEARLY' =>
      (cursor.year - start.year) % n == 0 &&
          (byDay.isNotEmpty ||
              byMonthDay.isNotEmpty ||
              (cursor.month == start.month && cursor.day == start.day)),
    _ => false,
  };
}

/// Expand the selected interval, applying cancellations before overrides.
/// The recurrence ID always identifies the original start, even after a move.
List<EventData> calendarOccurrences(
  List<EventData> events,
  DateTime first,
  DateTime end, {
  bool includeUndated = false,
}) {
  final result = <EventData>[];
  for (final master in events) {
    final start = calendarValueTime(master['dtstart']);
    if (start == null) {
      if (includeUndated) result.add({...master, 'master': master});
      continue;
    }
    final until = calendarValueTime(master['dtend']);
    final duration = until?.difference(start);
    final allDay = master['dtstart']['kind'] == 'date';
    final from = allDay
        ? DateTime.utc(first.year, first.month, first.day)
        : first.toUtc();
    final to = allDay
        ? DateTime.utc(end.year, end.month, end.day)
        : end.toUtc();
    final rule = master['rrule'];
    final byId = <int, EventData>{
      for (final ov
          in (master['overrides'] as List? ?? const []).whereType<EventData>())
        if (calendarValueTime(ov['recurrence_id']) case final key?)
          key.millisecondsSinceEpoch: ov,
    };
    final excluded = <int>{
      for (final value in master['exdates'] as List? ?? const [])
        if (calendarValueTime(value) case final key?)
          key.millisecondsSinceEpoch,
    };
    void emit(DateTime original, EventData? override) {
      if (excluded.contains(original.millisecondsSinceEpoch)) return;
      final begins = calendarValueTime(override?['dtstart']) ?? original;
      final finishes =
          calendarValueTime(override?['dtend']) ??
          begins.add(
            duration ?? (allDay ? _day : const Duration(milliseconds: 1)),
          );
      if (!begins.isBefore(to) || !finishes.isAfter(from)) return;
      result.add({
        ...master,
        if (override != null)
          for (final key in ['summary', 'location', 'description'])
            if (override[key] != null && override[key] != '')
              key: override[key],
        'dtstart': calendarValue(begins, allDay),
        if (duration != null || override?['dtend'] != null)
          'dtend': calendarValue(finishes, allDay),
        if (rule is Map) 'instance': calendarValue(original, allDay),
        'master': master,
      });
    }

    if (rule is! Map) {
      emit(start, null);
      continue;
    }
    final count = rule['count'] as int?;
    final stop = DateTime.tryParse('${rule['until']}')?.toUtc();
    var earliest = from.subtract(duration ?? (allDay ? _day : Duration.zero));
    var horizon = to;
    // A moved instance can enter the window from either direction. Its
    // original slot still has to satisfy COUNT, UNTIL and the recurrence rule.
    for (final entry in byId.entries) {
      final moved = calendarValueTime(entry.value['dtstart']);
      if (moved == null || !moved.isBefore(to)) continue;
      final finishes =
          calendarValueTime(entry.value['dtend']) ??
          moved.add(
            duration ?? (allDay ? _day : const Duration(milliseconds: 1)),
          );
      if (!finishes.isAfter(from)) continue;
      final original = DateTime.fromMillisecondsSinceEpoch(
        entry.key,
        isUtc: true,
      );
      if (original.isBefore(earliest)) earliest = original;
      if (!original.isBefore(horizon)) {
        horizon = original.add(const Duration(milliseconds: 1));
      }
    }
    var cursor = start;
    if (count == null && start.isBefore(earliest)) {
      cursor = start.add(Duration(days: earliest.difference(start).inDays));
    }
    var sequence = 0;
    while (cursor.isBefore(horizon) &&
        (stop == null || !cursor.isAfter(stop)) &&
        (count == null || sequence < count)) {
      if (_matches(cursor, start, rule)) {
        sequence++;
        emit(cursor, byId[cursor.millisecondsSinceEpoch]);
      }
      cursor = cursor.add(_day);
    }
  }
  result.sort(
    (a, b) => (calendarDisplayTime(a['dtstart'])?.millisecondsSinceEpoch ?? 0)
        .compareTo(
          calendarDisplayTime(b['dtstart'])?.millisecondsSinceEpoch ?? 0,
        ),
  );
  return result;
}

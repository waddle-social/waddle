import 'dart:convert';
import 'dart:typed_data';

import 'package:flutter/material.dart';

import '../bridge.dart';
import '../runtime/platform_file_access.dart';
import 'calendar_editor.dart';
import 'calendar_occurrences.dart';
import 'trip_plan.dart';

class EventCalendar extends StatefulWidget {
  const EventCalendar({
    super.key,
    required this.events,
    required this.bridge,
    required this.busy,
    required this.run,
  });
  final List<EventData> events;
  final WaddleBridge bridge;
  final bool busy;
  final Future<void> Function(Future<void> Function()) run;
  @override
  State<EventCalendar> createState() => _EventCalendarState();
}

class _EventCalendarState extends State<EventCalendar> {
  DateTime _selected = DateUtils.dateOnly(DateTime.now());
  bool _dayOnly = false;
  bool _attendingOnly = false;

  Future<void> _edit(EventData row, {required bool occurrence}) async {
    final input = await showDialog<EventData>(
      context: context,
      useRootNavigator: false,
      builder: (_) => CalendarEditor(
        event: occurrence ? row : row['master'],
        occurrence: occurrence,
      ),
    );
    if (!mounted || input == null || (input['fields'] as List).isEmpty) return;
    await widget.bridge.invokeQuiet('community.editEvent', {
      'id': row['id'],
      ...input,
      if (occurrence) 'occurrence': row['instance'],
    });
  }

  Future<void> _cancel(EventData row, {required bool occurrence}) async {
    final title = occurrence
        ? 'Cancel this occurrence?'
        : row['rrule'] == null
        ? 'Cancel this event?'
        : 'Cancel the entire series?';
    final confirmed = await showDialog<bool>(
      context: context,
      useRootNavigator: false,
      builder: (dialog) => AlertDialog(
        title: Text(title),
        content: Text('${row['summary']}'),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(dialog, false),
            child: const Text('Keep event'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(dialog, true),
            child: const Text('Confirm cancellation'),
          ),
        ],
      ),
    );
    if (!mounted || confirmed != true) return;
    await widget.bridge.invokeQuiet(
      occurrence ? 'community.editEvent' : 'community.cancelEvent',
      {
        'id': row['id'],
        if (occurrence) ...{
          'occurrence': row['instance'],
          'fields': <String>[],
        },
      },
    );
  }

  String _time(dynamic value) {
    final date = calendarDisplayTime(value);
    if (date == null) return '';
    final day = MaterialLocalizations.of(context).formatMediumDate(date);
    return value['kind'] == 'date'
        ? day
        : '$day · ${TimeOfDay.fromDateTime(date).format(context)}';
  }

  Widget _entry(EventData row) {
    final self = widget.bridge.session['jid'];
    final responses = row['responses'] as Map? ?? const {};
    final editable =
        row['organizer'] == 'xmpp:$self' ||
        widget.bridge.home['isCommunityOwner'] == true;
    final recurring = row['instance'] != null;
    final workspace = row['workspace_room'];
    final lines = [
      if (row['dtstart'] != null)
        '${_time(row['dtstart'])}${row['dtstart']['kind'] == 'date' ? ' · All day' : ''}',
      if (row['dtend'] != null)
        '${row['dtend']['kind'] == 'date' ? 'Ends before' : 'Ends'} ${_time(row['dtend'])}',
      if (row['location'] != null) '${row['location']}',
      if (row['description'] != null) '${row['description']}',
      if (recurring) 'Repeats ${row['rrule']['freq'].toString().toLowerCase()}',
      if (responses.isNotEmpty)
        "${responses.values.where((v) => v == 'ACCEPTED').length} going",
    ];
    return ExpansionTile(
      key: ValueKey((row['id'], calendarValueTime(row['instance']))),
      title: Text('${row['summary']}'),
      subtitle: Text(lines.join('\n')),
      children: [
        if (responses.isNotEmpty)
          for (final attendee in responses.entries)
            ListTile(
              dense: true,
              title: Text('${attendee.key}'),
              trailing: Text('${attendee.value}'),
            ),
        if (recurring) const Text('RSVP for the series'),
        Wrap(
          spacing: 4,
          children: [
            for (final response in {
              'ACCEPTED': 'Going',
              'TENTATIVE': 'Maybe',
              'DECLINED': 'Decline',
            }.entries)
              ChoiceChip(
                label: Text(response.value),
                selected: responses[self] == response.key,
                onSelected: widget.busy
                    ? null
                    : (_) => widget.run(() async {
                        await widget.bridge.invokeQuiet('community.rsvp', {
                          'id': row['uid'],
                          'response': response.key,
                        });
                      }),
              ),
          ],
        ),
        if (row['master']?['icalendar'] case final String calendar)
          TextButton.icon(
            icon: const Icon(Icons.event_outlined),
            label: Text(
              recurring ? 'Export series (.ics)' : 'Export event (.ics)',
            ),
            onPressed: widget.busy
                ? null
                : () => widget.run(() async {
                    final owner = widget.bridge.generation;
                    await openPrivateFile(
                      Uint8List.fromList(utf8.encode(calendar)),
                      'event.ics',
                      'text/calendar',
                      validate: () {
                        if (!mounted || owner != widget.bridge.generation) {
                          throw StateError('The account changed.');
                        }
                      },
                    );
                  }),
          ),
        if (workspace is String && workspace.isNotEmpty)
          TextButton.icon(
            icon: const Icon(Icons.map_outlined),
            label: const Text('Open trip plan'),
            onPressed: () => Navigator.push(
              context,
              MaterialPageRoute<void>(
                builder: (_) =>
                    TripPlanScreen(bridge: widget.bridge, room: workspace),
              ),
            ),
          ),
        if (editable)
          Wrap(
            children: [
              if (recurring)
                TextButton(
                  onPressed: widget.busy
                      ? null
                      : () => widget.run(() => _edit(row, occurrence: true)),
                  child: const Text('Edit occurrence'),
                ),
              TextButton(
                onPressed: widget.busy
                    ? null
                    : () => widget.run(() => _edit(row, occurrence: false)),
                child: Text(recurring ? 'Edit series' : 'Edit event'),
              ),
              if (recurring)
                TextButton(
                  onPressed: widget.busy
                      ? null
                      : () => widget.run(() => _cancel(row, occurrence: true)),
                  child: const Text('Cancel occurrence'),
                ),
              TextButton(
                onPressed: widget.busy
                    ? null
                    : () => widget.run(() => _cancel(row, occurrence: false)),
                child: Text(recurring ? 'Cancel series' : 'Cancel event'),
              ),
            ],
          ),
      ],
    );
  }

  @override
  Widget build(BuildContext context) {
    final from = _dayOnly
        ? _selected
        : DateTime(_selected.year, _selected.month);
    final to = _dayOnly
        ? DateTime(from.year, from.month, from.day + 1)
        : DateTime(from.year, from.month + 1);
    final rows = calendarOccurrences(
      _attendingOnly
          ? widget.events.where((event) {
              final response =
                  event['responses']?[widget.bridge.session['jid']];
              return response == 'ACCEPTED' || response == 'TENTATIVE';
            }).toList()
          : widget.events,
      from,
      to,
      includeUndated: !_dayOnly,
    );
    return ListView(
      children: [
        CalendarDatePicker(
          initialDate: _selected,
          firstDate: DateTime(1),
          lastDate: DateTime(9999, 12, 31),
          onDateChanged: (date) => setState(() {
            _selected = date;
            _dayOnly = true;
          }),
          onDisplayedMonthChanged: (date) => setState(() {
            _selected = DateTime(date.year, date.month);
            _dayOnly = false;
          }),
        ),
        Center(
          child: SegmentedButton<bool>(
            segments: const [
              ButtonSegment(value: false, label: Text('Month agenda')),
              ButtonSegment(value: true, label: Text('Selected day')),
            ],
            selected: {_dayOnly},
            onSelectionChanged: (values) =>
                setState(() => _dayOnly = values.single),
          ),
        ),
        Center(
          child: FilterChip(
            label: const Text('Going or maybe'),
            selected: _attendingOnly,
            onSelected: (value) => setState(() => _attendingOnly = value),
          ),
        ),
        if (rows.isEmpty)
          const Padding(
            padding: EdgeInsets.all(24),
            child: Center(child: Text('No events in this period')),
          ),
        for (final row in rows) _entry(row),
      ],
    );
  }
}

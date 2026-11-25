import 'package:flutter/material.dart';

import 'calendar_occurrences.dart';

class CalendarEditor extends StatefulWidget {
  const CalendarEditor({super.key, this.event, this.occurrence = false});
  final EventData? event;
  final bool occurrence;
  @override
  State<CalendarEditor> createState() => _CalendarEditorState();
}

class _CalendarEditorState extends State<CalendarEditor> {
  late final _title = TextEditingController(
    text: widget.event?['summary'] as String?,
  );
  late final _description = TextEditingController(
    text: widget.event?['description'] as String?,
  );
  late final _location = TextEditingController(
    text: widget.event?['location'] as String?,
  );
  late DateTime? _start = calendarDisplayTime(widget.event?['dtstart']);
  late DateTime? _end = calendarDisplayTime(widget.event?['dtend']);
  late bool _allDay = widget.event?['dtstart']?['kind'] == 'date';
  late final Map _initialRule = widget.event?['rrule'] as Map? ?? const {};
  late String _freq = _initialRule['freq'] as String? ?? 'Never';
  late final _interval = TextEditingController(
    text: '${_initialRule['interval'] ?? 1}',
  );
  late final _count = TextEditingController(
    text: '${_initialRule['count'] ?? ''}',
  );
  late final _weekdays = Set<String>.from(_initialRule['by_day'] ?? const []);
  late DateTime? _until = DateTime.tryParse(
    '${_initialRule['until']}',
  )?.toLocal();
  bool _ruleChanged = false;
  String? _error;

  @override
  void dispose() {
    for (final c in [_title, _description, _location, _interval, _count]) {
      c.dispose();
    }
    super.dispose();
  }

  Future<DateTime?> _pick(DateTime? current, {bool dateOnly = false}) async {
    final initial = current ?? DateTime.now();
    final first = initial.year < 1 ? initial : DateTime(1);
    final last = initial.year > 9999 ? initial : DateTime(9999, 12, 31);
    final day = await showDatePicker(
      context: context,
      initialDate: initial,
      firstDate: first,
      lastDate: last,
    );
    if (!mounted || day == null || dateOnly) return day;
    final time = await showTimePicker(
      context: context,
      initialTime: TimeOfDay.fromDateTime(initial),
    );
    return time == null
        ? null
        : DateTime(day.year, day.month, day.day, time.hour, time.minute);
  }

  String _label(DateTime? at, String empty, {bool dateOnly = false}) {
    if (at == null) return empty;
    final day = MaterialLocalizations.of(context).formatMediumDate(at);
    return dateOnly
        ? day
        : '$day · ${TimeOfDay.fromDateTime(at).format(context)}';
  }

  void _save() {
    String? error;
    final interval = int.tryParse(_interval.text);
    final count = int.tryParse(_count.text);
    if (_title.text.trim().isEmpty) {
      error = 'Enter an event name.';
    } else if (_start == null) {
      error = 'Choose a start date.';
    } else if (_end != null && !_end!.isAfter(_start!)) {
      error = 'The end must be after the start.';
    } else if (!widget.occurrence && _freq != 'Never' && _ruleChanged) {
      if (interval == null || interval < 1 || interval > 4294967295) {
        error = 'Enter a positive repeat interval.';
      } else if (_count.text.isNotEmpty &&
          (count == null || count < 1 || count > 4294967295)) {
        error = 'Enter a positive number of occurrences.';
      } else if (_until != null && _until!.isBefore(_start!)) {
        error = 'The last occurrence must follow the start.';
      }
    }
    if (error != null) {
      setState(() => _error = error);
      return;
    }
    final input = <String, dynamic>{
      'summary': _title.text.trim(),
      'description': _description.text.trim(),
      'location': _location.text.trim(),
      'dtstart': calendarValue(_start!, _allDay),
      'dtend': _end == null ? null : calendarValue(_end!, _allDay),
      if (!widget.occurrence)
        'rrule': _freq == 'Never'
            ? null
            : !_ruleChanged
            ? _initialRule
            : {
                ..._initialRule,
                'freq': _freq,
                'interval': interval,
                'count': count,
                'until': _until?.toUtc().toIso8601String(),
                'by_day': _weekdays.toList(),
              },
    };
    final source = widget.event;
    final fields = [
      for (final entry in input.entries)
        if (source == null ||
            (entry.key == 'rrule'
                ? _ruleChanged
                : (entry.key == 'dtstart' || entry.key == 'dtend')
                ? ((source[entry.key]?['kind'] !=
                          (entry.value as Map?)?['kind']) ||
                      calendarValueTime(source[entry.key]) !=
                          calendarValueTime(entry.value))
                : (source[entry.key] ?? '') != entry.value))
          entry.key,
    ];
    Navigator.pop(context, {'event': input, 'fields': fields});
  }

  @override
  Widget build(BuildContext context) => AlertDialog(
    title: Text(
      widget.event == null
          ? 'New event'
          : widget.occurrence
          ? 'Edit occurrence'
          : 'Edit event',
    ),
    content: SizedBox(
      width: 400,
      child: SingleChildScrollView(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            TextField(
              controller: _title,
              autofocus: true,
              decoration: const InputDecoration(labelText: 'Event name'),
            ),
            TextField(
              controller: _location,
              decoration: const InputDecoration(labelText: 'Location'),
            ),
            TextField(
              controller: _description,
              maxLines: 2,
              decoration: const InputDecoration(labelText: 'Description'),
            ),
            SwitchListTile(
              contentPadding: EdgeInsets.zero,
              title: const Text('All day'),
              value: _allDay,
              onChanged: widget.occurrence
                  ? null
                  : (value) => setState(() {
                      _allDay = value;
                      if (value) {
                        if (_start != null) {
                          _start = DateUtils.dateOnly(_start!);
                        }
                        if (_end != null) {
                          _end = DateUtils.dateOnly(_end!);
                          if (_start != null && !_end!.isAfter(_start!)) {
                            _end = DateTime(
                              _start!.year,
                              _start!.month,
                              _start!.day + 1,
                            );
                          }
                        }
                        if (_until != null) {
                          _until = null;
                          _ruleChanged = true;
                        }
                      }
                    }),
            ),
            ListTile(
              contentPadding: EdgeInsets.zero,
              title: const Text('Starts'),
              subtitle: Text(_label(_start, 'Choose start', dateOnly: _allDay)),
              onTap: () async {
                final time = await _pick(_start, dateOnly: _allDay);
                if (mounted && time != null) setState(() => _start = time);
              },
            ),
            ListTile(
              contentPadding: EdgeInsets.zero,
              title: Text(_allDay ? 'Ends before' : 'Ends'),
              subtitle: Text(
                _label(_end, 'No end specified', dateOnly: _allDay),
              ),
              trailing: _end == null
                  ? null
                  : IconButton(
                      tooltip: 'Remove end',
                      icon: const Icon(Icons.clear),
                      onPressed: () => setState(() => _end = null),
                    ),
              onTap: () async {
                final time = await _pick(_end ?? _start, dateOnly: _allDay);
                if (mounted && time != null) setState(() => _end = time);
              },
            ),
            if (!widget.occurrence) ...[
              DropdownButtonFormField<String>(
                initialValue: _freq,
                decoration: const InputDecoration(labelText: 'Repeat'),
                items: [
                  for (final key in [
                    'Never',
                    'DAILY',
                    'WEEKLY',
                    'MONTHLY',
                    'YEARLY',
                  ])
                    DropdownMenuItem(
                      value: key,
                      child: Text(key[0] + key.substring(1).toLowerCase()),
                    ),
                ],
                onChanged: (v) => setState(() {
                  _freq = v!;
                  _ruleChanged = true;
                }),
              ),
              if (_freq != 'Never') ...[
                TextField(
                  controller: _interval,
                  keyboardType: TextInputType.number,
                  decoration: const InputDecoration(
                    labelText: 'Repeat interval',
                  ),
                  onChanged: (_) => _ruleChanged = true,
                ),
                if (_freq == 'WEEKLY')
                  Wrap(
                    children: [
                      for (final d in [
                        'MO',
                        'TU',
                        'WE',
                        'TH',
                        'FR',
                        'SA',
                        'SU',
                      ])
                        FilterChip(
                          label: Text(d),
                          selected: _weekdays.contains(d),
                          onSelected: (add) => setState(() {
                            _ruleChanged = true;
                            if (add) {
                              _weekdays.add(d);
                            } else {
                              _weekdays.remove(d);
                            }
                          }),
                        ),
                    ],
                  ),
                TextField(
                  controller: _count,
                  keyboardType: TextInputType.number,
                  decoration: const InputDecoration(
                    labelText: 'Occurrence limit',
                  ),
                  onChanged: (value) => setState(() {
                    _ruleChanged = true;
                    if (value.isNotEmpty) _until = null;
                  }),
                ),
                if (!_allDay)
                  ListTile(
                    contentPadding: EdgeInsets.zero,
                    title: const Text('Repeat until'),
                    subtitle: Text(_label(_until, 'No end date')),
                    trailing: _until == null
                        ? null
                        : IconButton(
                            tooltip: 'Remove repeat end',
                            icon: const Icon(Icons.clear),
                            onPressed: () => setState(() {
                              _until = null;
                              _ruleChanged = true;
                            }),
                          ),
                    onTap: () async {
                      final v = await _pick(_until ?? _start);
                      if (mounted && v != null) {
                        setState(() {
                          _until = v;
                          _count.clear();
                          _ruleChanged = true;
                        });
                      }
                    },
                  ),
              ],
            ],
            if (_error != null)
              Text(
                _error!,
                style: TextStyle(color: Theme.of(context).colorScheme.error),
              ),
          ],
        ),
      ),
    ),
    actions: [
      TextButton(
        onPressed: () => Navigator.pop(context),
        child: const Text('Cancel'),
      ),
      FilledButton(onPressed: _save, child: const Text('Save event')),
    ],
  );
}

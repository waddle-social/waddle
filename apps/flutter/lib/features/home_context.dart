part of '../app.dart';

EventData? _nextEvent(List<EventData> events, DateTime now) {
  final first = DateTime(now.year, now.month, now.day);
  final horizon = first.add(const Duration(days: 366));
  final occurrences = calendarOccurrences(events, first, horizon);
  final futureMasters = events.where((event) {
    final start = calendarDisplayTime(event['dtstart']);
    return event['rrule'] == null && start != null && !start.isBefore(horizon);
  });
  EventData? next;
  DateTime? nextStart;
  for (final event in [...occurrences, ...futureMasters]) {
    final start = calendarDisplayTime(event['dtstart']);
    if (start == null) continue;
    final end =
        calendarDisplayTime(event['dtend']) ??
        (event['dtstart']['kind'] == 'date'
            ? DateTime(start.year, start.month, start.day + 1)
            : start.add(const Duration(milliseconds: 1)));
    if (end.isAfter(now) && (nextStart == null || start.isBefore(nextStart))) {
      next = event;
      nextStart = start;
    }
  }
  return next;
}

class _HomeContext extends StatefulWidget {
  const _HomeContext({
    required this.bridge,
    required this.people,
    required this.open,
    required this.calendar,
  });
  final WaddleBridge bridge;
  final List<Map<String, dynamic>> people;
  final _OpenEntry open;
  final VoidCallback calendar;
  @override
  State<_HomeContext> createState() => _HomeContextState();
}

class _HomeContextState extends State<_HomeContext> with RouteAware {
  late Future<dynamic> _events;
  PageRoute<dynamic>? _route;
  String _filter = '';
  bool _showAway = false;
  @override
  void initState() {
    super.initState();
    _events = widget.bridge.invokeQuiet('community.events');
  }

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    final route = ModalRoute.of(context);
    if (route is PageRoute && route != _route) {
      waddleRoutes.unsubscribe(this);
      _route = route;
      waddleRoutes.subscribe(this, route);
    }
  }

  @override
  void didPopNext() =>
      setState(() => _events = widget.bridge.invokeQuiet('community.events'));
  @override
  void dispose() {
    waddleRoutes.unsubscribe(this);
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final c = Theme.of(context).colorScheme;
    final people = widget.people
        .where(
          (p) => '${p['name']} ${p['jid']}'.toLowerCase().contains(
            _filter.trim().toLowerCase(),
          ),
        )
        .toList();
    Widget section(
      String title,
      List<Map<String, dynamic>> rows, {
      String? empty,
      bool collapse = false,
    }) => Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        const SizedBox(height: 20),
        if (collapse)
          InkWell(
            onTap: () => setState(() => _showAway = !_showAway),
            child: Row(
              children: [
                Expanded(child: _Kicker('$title · ${rows.length}')),
                Icon(
                  _showAway ? Icons.expand_less : Icons.expand_more,
                  size: 18,
                ),
              ],
            ),
          )
        else
          _Kicker('$title · ${rows.length}', live: title == 'In a huddle'),
        if (!collapse || _showAway) ...[
          if (rows.isEmpty && empty != null)
            Padding(
              padding: const EdgeInsets.only(top: 7),
              child: Text(
                empty,
                style: Theme.of(
                  context,
                ).textTheme.bodySmall?.copyWith(color: c.onSurfaceVariant),
              ),
            ),
          for (final p in rows)
            _SidebarRow(
              title: _entryName(p),
              leading: _AccountAvatar(
                jid: p['jid'] as String,
                name: _entryName(p),
                size: 26,
                online: _around(p),
              ),
              onTap: () => widget.open(p, false),
            ),
        ],
      ],
    );
    return ColoredBox(
      color: c.surfaceContainerLow,
      child: ListView(
        padding: const EdgeInsets.all(16),
        children: [
          _DashboardCard(
            child: Padding(
              padding: const EdgeInsets.all(16),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  const _Kicker('Next event'),
                  const SizedBox(height: 8),
                  FutureBuilder<dynamic>(
                    future: _events,
                    builder: (context, snapshot) {
                      if (snapshot.connectionState != ConnectionState.done) {
                        return const LinearProgressIndicator();
                      }
                      if (snapshot.hasError) {
                        return TextButton(
                          onPressed: () => setState(
                            () => _events = widget.bridge.invokeQuiet(
                              'community.events',
                            ),
                          ),
                          child: const Text('Retry loading events'),
                        );
                      }
                      final next = _nextEvent(
                        records(snapshot.data),
                        DateTime.now(),
                      );
                      if (next == null) {
                        return Text(
                          'Nothing scheduled yet. Events you post show up here.',
                          style: Theme.of(context).textTheme.bodySmall
                              ?.copyWith(color: c.onSurfaceVariant),
                        );
                      }
                      final date = calendarDisplayTime(next['dtstart'])!;
                      final l = MaterialLocalizations.of(context);
                      return Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          Text(
                            '${next['summary'] ?? 'Community event'}',
                            style: Theme.of(context).textTheme.titleMedium,
                          ),
                          Text(
                            [
                              l.formatShortDate(date),
                              if (next['dtstart']['kind'] != 'date')
                                l.formatTimeOfDay(TimeOfDay.fromDateTime(date)),
                            ].join(' · '),
                          ),
                          TextButton(
                            onPressed: widget.calendar,
                            child: const Text('See events'),
                          ),
                        ],
                      );
                    },
                  ),
                ],
              ),
            ),
          ),
          const SizedBox(height: 20),
          TextField(
            decoration: const InputDecoration(
              hintText: 'Find people',
              prefixIcon: Icon(Icons.search, size: 18),
              isDense: true,
            ),
            onChanged: (v) => setState(() => _filter = v),
          ),
          section(
            'In a huddle',
            people.where((p) => p['inHuddle'] == true).toList(),
            empty: 'Nobody is in a huddle right now. Start one.',
          ),
          section(
            'Around',
            people.where(_around).toList(),
            empty: 'Nobody around right now.',
          ),
          section(
            'Away and offline',
            people.where((p) => !_around(p)).toList(),
            collapse: true,
          ),
        ],
      ),
    );
  }
}

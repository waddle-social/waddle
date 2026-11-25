import 'dart:math';

import 'package:flutter/material.dart';

import '../bridge.dart';

part 'trip_plan_editor.dart';
part 'trip_plans.dart';

const _kinds = {
  'event': 'Overview',
  'itinerary': 'Itinerary',
  'task': 'Task',
  'travel': 'Travel',
  'expense': 'Expense',
  'decision': 'Decision',
  'link': 'Link',
  'poll': 'Poll',
};
const _answers = {
  'going': 'Going',
  'interested': 'Interested',
  'declined': 'Declined',
};

typedef _Item = Map<String, dynamic>;

String _newId() {
  final random = Random.secure();
  return List.generate(
    16,
    (_) => random.nextInt(256).toRadixString(16).padLeft(2, '0'),
  ).join();
}

class TripPlanScreen extends StatefulWidget {
  const TripPlanScreen({super.key, required this.bridge, required this.room});
  final WaddleBridge bridge;
  final String room;

  @override
  State<TripPlanScreen> createState() => _TripPlanState();
}

class _TripPlanState extends State<TripPlanScreen> {
  late Future<List<_Item>> _items;
  bool _busy = false;

  @override
  void initState() {
    super.initState();
    _items = _read();
  }

  Future<List<_Item>> _read() async {
    final raw = await widget.bridge.invokeQuiet('planning.list', {
      'room': widget.room,
    });
    if (raw is! List) throw StateError('The room returned an invalid plan.');
    return raw.map((row) => _Item.from(row as Map)).toList();
  }

  void _refresh() => setState(() => _items = _read());

  Future<void> _write(String method, _Item args) async {
    if (_busy) return;
    setState(() => _busy = true);
    try {
      await widget.bridge.invokeQuiet(method, {'room': widget.room, ...args});
      if (mounted) _refresh();
    } catch (error) {
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
          SnackBar(content: Text(WaddleBridge.describeError(error))),
        );
      }
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _save(_Item item) => _write('planning.save', {'item': item});

  Future<void> _edit(String kind, [_Item? item]) async {
    final result = await showDialog<_Item>(
      context: context,
      builder: (_) => _PlanEditor(kind: kind, item: item),
    );
    if (mounted && result != null) await _save(result);
  }

  Future<void> _remove(_Item item) async {
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text('Remove ${item['title']}?'),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context),
            child: const Text('Cancel'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(context, true),
            child: const Text('Remove'),
          ),
        ],
      ),
    );
    if (mounted && confirmed == true) {
      await _write('planning.remove', {'id': item['id']});
    }
  }

  Widget _card(_Item item, List<_Item> rows) {
    final self = widget.bridge.session['jid'];
    final kind = item['kind'];
    final votes = rows.where(
      (row) => row['kind'] == 'vote' && row['subject'] == item['id'],
    );
    int voteCount(Object? option) => votes
        .where((row) => (row['selections'] as List).contains(option))
        .length;
    final subtitle = [
      _kinds[kind],
      if (item['completed'] == true) 'Completed',
      item['description'],
      item['location'],
      item['assignee'],
      item['status'],
      if (item['starts_at'] != null)
        '${item['starts_at']}${item['ends_at'] == null ? '' : ' – ${item['ends_at']}'}',
      if (item['currency'] != null)
        '${item['amount_minor']} minor units · ${item['currency']}',
      ...?item['participants'] as List?,
    ].whereType<String>().where((text) => text.isNotEmpty).join('\n');
    return ExpansionTile(
      key: PageStorageKey(item['id']),
      leading: item['pinned'] == true
          ? const Icon(Icons.push_pin_outlined)
          : null,
      title: Text('${item['title']}'),
      subtitle: Text(subtitle),
      children: [
        if (item['url'] case final String url)
          TextButton(
            onPressed: () => widget.bridge.invoke('link.open', {'url': url}),
            child: Text(url),
          ),
        if (kind == 'poll')
          for (final option
              in (item['options'] as List? ?? const []).whereType<Map>())
            ListTile(
              title: Text('${option['label']}'),
              subtitle: Text(
                '${voteCount(option['id'])} ${voteCount(option['id']) == 1 ? 'vote' : 'votes'}',
              ),
              selected: votes.any(
                (row) =>
                    row['author'] == self &&
                    (row['selections'] as List).contains(option['id']),
              ),
              onTap: _busy
                  ? null
                  : () => _save({
                      'id': 'vote:${item['id']}:$self',
                      'kind': 'vote',
                      'title': '',
                      'subject': item['id'],
                      'selections': [option['id']],
                    }),
            ),
        Wrap(
          children: [
            if (kind == 'task')
              TextButton(
                onPressed: _busy
                    ? null
                    : () => _save({
                        ...item,
                        'completed': item['completed'] != true,
                      }),
                child: Text(
                  item['completed'] == true ? 'Reopen task' : 'Complete task',
                ),
              ),
            TextButton(
              onPressed: _busy ? null : () => _edit(kind as String, item),
              child: const Text('Edit'),
            ),
            TextButton(
              onPressed: _busy
                  ? null
                  : () => _save({...item, 'pinned': item['pinned'] != true}),
              child: Text(item['pinned'] == true ? 'Unpin' : 'Pin'),
            ),
            TextButton(
              onPressed: _busy ? null : () => _remove(item),
              child: const Text('Remove'),
            ),
          ],
        ),
      ],
    );
  }

  @override
  Widget build(BuildContext context) => Scaffold(
    appBar: AppBar(
      title: const Text('Trip plan'),
      actions: [
        IconButton(
          tooltip: 'Refresh',
          onPressed: _refresh,
          icon: const Icon(Icons.refresh),
        ),
      ],
    ),
    body: FutureBuilder<List<_Item>>(
      future: _items,
      builder: (context, snapshot) {
        if (snapshot.connectionState != ConnectionState.done) {
          return const Center(child: CircularProgressIndicator());
        }
        if (snapshot.hasError) {
          return Center(
            child: Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                Text(WaddleBridge.describeError(snapshot.error!)),
                TextButton(onPressed: _refresh, child: const Text('Retry')),
              ],
            ),
          );
        }
        final rows = snapshot.requireData;
        final attendance = rows.where((row) => row['kind'] == 'attendance');
        final self = widget.bridge.session['jid'];
        final items =
            rows.where((row) => _kinds.containsKey(row['kind'])).toList()
              ..sort((a, b) {
                int rank(_Item item) => item['kind'] == 'event'
                    ? 0
                    : item['pinned'] == true
                    ? 1
                    : 2;
                final order = rank(a).compareTo(rank(b));
                return order == 0
                    ? '${a['starts_at'] ?? ''}${a['title']}'.compareTo(
                        '${b['starts_at'] ?? ''}${b['title']}',
                      )
                    : order;
              });
        return ListView(
          padding: const EdgeInsets.all(16),
          children: [
            PopupMenuButton<String>(
              enabled: !_busy,
              onSelected: _edit,
              tooltip: 'Add to trip plan',
              itemBuilder: (_) => [
                for (final entry in _kinds.entries)
                  if (entry.key != 'event' ||
                      !items.any((row) => row['kind'] == 'event'))
                    PopupMenuItem(value: entry.key, child: Text(entry.value)),
              ],
              child: const ListTile(
                leading: Icon(Icons.add),
                title: Text('Add to trip plan'),
              ),
            ),
            for (final item in items) _card(item, rows),
            const ListTile(title: Text('Attendance')),
            Wrap(
              spacing: 8,
              children: [
                for (final answer in _answers.entries)
                  ChoiceChip(
                    label: Text(answer.value),
                    selected: attendance.any(
                      (row) =>
                          row['author'] == self &&
                          row['attendance'] == answer.key,
                    ),
                    onSelected: _busy
                        ? null
                        : (_) => _save({
                            'id': 'attendance:$self',
                            'kind': 'attendance',
                            'title': '',
                            'attendance': answer.key,
                          }),
                  ),
              ],
            ),
            for (final row in attendance)
              ListTile(
                title: Text('${row['author']}'),
                trailing: Text(
                  _answers[row['attendance']] ?? '${row['attendance']}',
                ),
              ),
          ],
        );
      },
    ),
  );
}

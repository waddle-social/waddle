import 'dart:async';

import 'package:flutter/material.dart';

import '../bridge.dart';

enum CommunitySection {
  feed('Feed', Icons.article_outlined, 'community.feed', 'New post'),
  stories(
    'Stories',
    Icons.photo_library_outlined,
    'community.stories',
    'New story',
  ),
  events('Events', Icons.event_outlined, 'community.events', 'New event');

  const CommunitySection(this.title, this.icon, this.query, this.compose);
  final String title, query, compose;
  final IconData icon;
}

class CommunityScreen extends StatefulWidget {
  const CommunityScreen({
    super.key,
    required this.bridge,
    required this.section,
  });
  final WaddleBridge bridge;
  final CommunitySection section;

  @override
  State<CommunityScreen> createState() => _CommunityScreenState();
}

class _CommunityScreenState extends State<CommunityScreen> {
  late Future<List<Map<String, dynamic>>> _items;
  Timer? _expiry;
  bool _busy = false;

  @override
  void initState() {
    super.initState();
    _items = _load();
  }

  @override
  void dispose() {
    _expiry?.cancel();
    super.dispose();
  }

  Future<List<Map<String, dynamic>>> _load() async {
    final result = await widget.bridge.invokeQuiet(widget.section.query);
    if (result is! List) {
      throw StateError('The server returned an invalid list.');
    }
    final items = result
        .whereType<Map>()
        .map((item) => Map<String, dynamic>.from(item))
        .toList();
    return items;
  }

  void _reload() => setState(() => _items = _load());

  Future<void> _action(Future<void> Function() action) async {
    if (_busy) return;
    setState(() => _busy = true);
    try {
      await action();
      if (mounted) _reload();
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

  Future<void> _compose() async {
    final input = await showDialog<Map<String, dynamic>>(
      context: context,
      builder: (_) => _CommunityComposer(section: widget.section),
    );
    if (!mounted || input == null) return;
    final command = switch (widget.section) {
      CommunitySection.feed => 'community.post',
      CommunitySection.stories => 'community.publishStory',
      CommunitySection.events => 'community.createEvent',
    };
    await widget.bridge.invokeQuiet(command, input);
  }

  Future<void> _openStory(Map<String, dynamic> row) async {
    final url = Uri.tryParse('${row['media_url']}');
    if (url?.scheme != 'https' ||
        url!.host.isEmpty ||
        url.userInfo.isNotEmpty) {
      throw StateError('This story has no usable media link.');
    }
    await widget.bridge.invokeQuiet('link.open', {'url': url.toString()});
    if (mounted) {
      await widget.bridge.invokeQuiet('community.readStory', {'id': row['id']});
    }
  }

  String _start(dynamic raw) {
    if (raw is! Map) return '';
    if (raw['kind'] == 'date') return raw['date']?.toString() ?? '';
    final ms = raw['ms'];
    if (ms is! num) return '';
    final date = DateTime.fromMillisecondsSinceEpoch(ms.round());
    final locale = MaterialLocalizations.of(context);
    return '${locale.formatMediumDate(date)} · ${TimeOfDay.fromDateTime(date).format(context)}';
  }

  Widget _entry(Map<String, dynamic> row) {
    switch (widget.section) {
      case CommunitySection.feed:
        return ListTile(
          title: Text('${row['title'] ?? row['author'] ?? 'Community post'}'),
          subtitle: SelectableText('${row['body'] ?? ''}'),
        );
      case CommunitySection.stories:
        return ListTile(
          leading: Icon(
            row['read'] == true
                ? Icons.check_circle_outline
                : Icons.auto_stories_outlined,
          ),
          title: Text('${row['author'] ?? 'Story'}'),
          subtitle: Text('${row['body'] ?? 'View media'}'),
          onTap: _busy ? null : () => _action(() => _openStory(row)),
        );
      case CommunitySection.events:
        final responses = row['responses'] as Map? ?? const {};
        final self = widget.bridge.session['jid'];
        final lines = [
          _start(row['dtstart']),
          if (row['rrule'] is Map)
            'Repeats ${row['rrule']['freq'].toString().toLowerCase()}',
          if (row['location'] != null) '${row['location']}',
          if (row['description'] != null) '${row['description']}',
          if (responses.isNotEmpty)
            "${responses.values.where((value) => value == 'ACCEPTED').length} going",
        ];
        return ExpansionTile(
          title: Text('${row['summary'] ?? 'Event'}'),
          subtitle: Text(lines.where((line) => line.isNotEmpty).join('\n')),
          children: [
            Wrap(
              children: [
                for (final response in {
                  'ACCEPTED': 'Going',
                  'TENTATIVE': 'Maybe',
                  'DECLINED': 'Decline',
                }.entries)
                  Padding(
                    padding: const EdgeInsets.all(4),
                    child: ChoiceChip(
                      selected: responses[self] == response.key,
                      label: Text(response.value),
                      onSelected: _busy
                          ? null
                          : (_) => _action(() async {
                              await widget.bridge.invokeQuiet(
                                'community.rsvp',
                                {'id': row['uid'], 'response': response.key},
                              );
                            }),
                    ),
                  ),
              ],
            ),
            if (row['organizer'] == 'xmpp:$self' && row['id'] == row['uid'])
              TextButton(
                onPressed: _busy
                    ? null
                    : () => _action(() async {
                        final confirmed = await showDialog<bool>(
                          context: context,
                          builder: (context) => AlertDialog(
                            title: const Text(
                              'Cancel this event and all its occurrences?',
                            ),
                            actions: [
                              TextButton(
                                onPressed: () => Navigator.pop(context, false),
                                child: const Text('Keep event'),
                              ),
                              TextButton(
                                onPressed: () => Navigator.pop(context, true),
                                child: const Text('Cancel event'),
                              ),
                            ],
                          ),
                        );
                        if (mounted && confirmed == true) {
                          await widget.bridge.invokeQuiet(
                            'community.cancelEvent',
                            {'id': row['id']},
                          );
                        }
                      }),
                child: const Text('Cancel event'),
              ),
          ],
        );
    }
  }

  List<Map<String, dynamic>> _visibleStories(List<Map<String, dynamic>> rows) {
    final now = DateTime.now();
    DateTime? next;
    final visible = rows.where((row) {
      final at = DateTime.tryParse('${row['expires']}');
      if (at == null || !at.isAfter(now)) return false;
      if (next == null || at.isBefore(next!)) next = at;
      return true;
    }).toList();
    _expiry?.cancel();
    if (next != null) {
      _expiry = Timer(next!.difference(now), () {
        if (mounted) setState(() {});
      });
    }
    return visible;
  }

  @override
  Widget build(BuildContext context) => Scaffold(
    appBar: AppBar(
      title: Text(widget.section.title),
      actions: [
        IconButton(
          tooltip: 'Refresh',
          onPressed: _reload,
          icon: const Icon(Icons.refresh),
        ),
        IconButton(
          tooltip: widget.section.compose,
          onPressed: _busy ? null : () => _action(_compose),
          icon: const Icon(Icons.add),
        ),
      ],
    ),
    body: FutureBuilder<List<Map<String, dynamic>>>(
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
                TextButton(onPressed: _reload, child: const Text('Retry')),
              ],
            ),
          );
        }
        final rows = widget.section == CommunitySection.stories
            ? _visibleStories(snapshot.data!)
            : snapshot.data!;
        if (rows.isEmpty) {
          return Center(
            child: Text(switch (widget.section) {
              CommunitySection.events => 'No community events',
              CommunitySection.stories => 'No active stories',
              CommunitySection.feed => 'No posts yet',
            }),
          );
        }
        return ListView.separated(
          itemCount: rows.length,
          separatorBuilder: (_, _) => const Divider(height: 1),
          itemBuilder: (_, index) => _entry(rows[index]),
        );
      },
    ),
  );
}

class _CommunityComposer extends StatefulWidget {
  const _CommunityComposer({required this.section});
  final CommunitySection section;

  @override
  State<_CommunityComposer> createState() => _CommunityComposerState();
}

class _CommunityComposerState extends State<_CommunityComposer> {
  String _text = '';
  DateTime? _start;
  String? _repeat;

  Future<void> _pickDate() async {
    final now = DateTime.now();
    final date = await showDatePicker(
      context: context,
      initialDate: _start ?? now,
      firstDate: DateTime(now.year),
      lastDate: DateTime(now.year + 10),
    );
    if (!mounted || date == null) return;
    final time = await showTimePicker(
      context: context,
      initialTime: TimeOfDay.fromDateTime(_start ?? now),
    );
    if (mounted && time != null) {
      setState(
        () => _start = DateTime(
          date.year,
          date.month,
          date.day,
          time.hour,
          time.minute,
        ),
      );
    }
  }

  @override
  Widget build(BuildContext context) {
    final story = widget.section == CommunitySection.stories,
        event = widget.section == CommunitySection.events;
    return AlertDialog(
      title: Text(widget.section.compose),
      content: SingleChildScrollView(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            TextField(
              autofocus: true,
              maxLines: event ? 1 : 3,
              decoration: InputDecoration(
                labelText: event
                    ? 'Event name'
                    : story
                    ? 'Caption (optional)'
                    : 'Post',
              ),
              onChanged: (value) => setState(() => _text = value.trim()),
            ),
            if (event) ...[
              TextButton(
                onPressed: _pickDate,
                child: Text(
                  _start == null
                      ? 'Choose start time'
                      : '${MaterialLocalizations.of(context).formatMediumDate(_start!)} · ${TimeOfDay.fromDateTime(_start!).format(context)}',
                ),
              ),
              DropdownButtonFormField<String>(
                decoration: const InputDecoration(labelText: 'Repeat'),
                initialValue: 'Never',
                items: ['Never', 'Daily', 'Weekly', 'Monthly', 'Yearly']
                    .map(
                      (value) =>
                          DropdownMenuItem(value: value, child: Text(value)),
                    )
                    .toList(),
                onChanged: (value) => setState(
                  () =>
                      _repeat = value == 'Never' ? null : value?.toUpperCase(),
                ),
              ),
            ],
          ],
        ),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.pop(context),
          child: const Text('Cancel'),
        ),
        FilledButton(
          onPressed: (!story && _text.isEmpty) || (event && _start == null)
              ? null
              : () => Navigator.pop(context, {
                  if (event) ...{
                    'summary': _text,
                    'starts': _start!.millisecondsSinceEpoch,
                    'repeat': _repeat,
                  } else
                    'body': _text,
                }),
          child: Text(story ? 'Choose image to publish' : 'Publish'),
        ),
      ],
    );
  }
}

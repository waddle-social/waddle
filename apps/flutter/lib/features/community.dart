import 'dart:async';

import 'package:flutter/material.dart';

import '../bridge.dart';
import 'story_viewer.dart';
import 'calendar_editor.dart';
import 'story_composer.dart';
import 'event_calendar.dart';

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
    final owner = widget.bridge.generation;
    final input = await showDialog<Map<String, dynamic>>(
      context: context,
      useRootNavigator: false,
      builder: (_) => widget.section == CommunitySection.events
          ? const CalendarEditor()
          : widget.section == CommunitySection.stories
          ? StoryComposer(bridge: widget.bridge)
          : const _FeedComposer(),
    );
    if (!mounted || widget.bridge.generation != owner || input == null) return;
    final command = switch (widget.section) {
      CommunitySection.feed => 'community.post',
      CommunitySection.stories => 'community.publishStory',
      CommunitySection.events => 'community.createEvent',
    };
    await widget.bridge.invokeQuiet(command, input);
  }

  Future<void> _reactToStory(Map<String, dynamic> row) async {
    final target = {'author': row['author'], 'id': row['id']};
    final data = await widget.bridge.invokeQuiet(
      'community.storyReactions',
      target,
    );
    if (!mounted) return;
    final mine = Set<String>.from(data['mine'] ?? const []);
    final counts = Map<String, dynamic>.from(data['counts'] ?? const {});
    final next = await showDialog<Set<String>>(
      context: context,
      useRootNavigator: false,
      builder: (dialog) => StatefulBuilder(
        builder: (context, update) {
          final choices = {
            ...counts.keys,
            ...mine,
            '❤️',
            '👍',
            '😂',
            '🎉',
            '😮',
            '🐧',
          };
          return AlertDialog(
            title: const Text('Story reactions'),
            content: Wrap(
              spacing: 8,
              children: [
                for (final emoji in choices)
                  FilterChip(
                    label: Text('$emoji ${counts[emoji] ?? 0}'),
                    selected: mine.contains(emoji),
                    onSelected: (selected) => update(() {
                      if (selected) {
                        mine.add(emoji);
                      } else {
                        mine.remove(emoji);
                      }
                    }),
                  ),
              ],
            ),
            actions: [
              TextButton(
                onPressed: () => Navigator.pop(dialog),
                child: const Text('Cancel'),
              ),
              FilledButton(
                onPressed: () => Navigator.pop(dialog, mine),
                child: const Text('Save reactions'),
              ),
            ],
          );
        },
      ),
    );
    if (next != null && mounted) {
      await widget.bridge.invokeQuiet('community.setStoryReactions', {
        ...target,
        'emojis': next.toList(),
      });
    }
  }

  Widget _entry(List<Map<String, dynamic>> rows, int index) {
    final row = rows[index];
    switch (widget.section) {
      case CommunitySection.feed:
        const sources = {
          'mood': 'Mood',
          'activity': 'Activity',
          'tune': 'Listening',
          'avatar': 'New avatar',
          'vcard': 'Profile update',
          'rsvp': 'Event RSVP',
        };
        final link = Uri.tryParse('${row['link'] ?? ''}');
        return ListTile(
          title: Text('${row['title'] ?? row['author'] ?? 'Community post'}'),
          subtitle: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              if (sources[row['source']] case final String source)
                Text(source, style: Theme.of(context).textTheme.labelSmall),
              SelectableText('${row['body'] ?? ''}'),
              if (link != null &&
                  link.scheme == 'https' &&
                  link.host.isNotEmpty &&
                  link.userInfo.isEmpty &&
                  RegExp(
                    r'\.(png|jpe?g|gif|webp|avif)$',
                    caseSensitive: false,
                  ).hasMatch(link.path))
                Image.network(
                  link.toString(),
                  height: 160,
                  fit: BoxFit.contain,
                  cacheWidth: 640,
                  errorBuilder: (_, _, _) => const SizedBox.shrink(),
                ),
              if (link != null &&
                  {'https', 'http'}.contains(link.scheme) &&
                  link.host.isNotEmpty &&
                  link.userInfo.isEmpty)
                TextButton(
                  onPressed: () => widget.bridge.invoke('link.open', {
                    'url': link.toString(),
                  }),
                  child: Text(link.toString()),
                ),
            ],
          ),
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
          trailing: IconButton(
            tooltip: 'Story reactions',
            icon: const Icon(Icons.add_reaction_outlined),
            onPressed: _busy ? null : () => _action(() => _reactToStory(row)),
          ),
          onTap: _busy
              ? null
              : () => _action(
                  () => Navigator.of(context).push<void>(
                    MaterialPageRoute(
                      builder: (_) => StoryViewer(
                        bridge: widget.bridge,
                        stories: rows,
                        initialIndex: index,
                      ),
                    ),
                  ),
                ),
        );
      case CommunitySection.events:
        throw StateError('Events use EventCalendar');
    }
  }

  List<Map<String, dynamic>> _visibleStories(List<Map<String, dynamic>> rows) {
    final now = DateTime.now();
    DateTime? next;
    final visible = rows.where((row) {
      final at = DateTime.tryParse('${row['expires']}');
      if (row['expires'] == null) return true;
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
        if (widget.section == CommunitySection.events) {
          return EventCalendar(
            events: snapshot.data!,
            bridge: widget.bridge,
            busy: _busy,
            run: _action,
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
          itemBuilder: (_, index) => _entry(rows, index),
        );
      },
    ),
  );
}

class _FeedComposer extends StatefulWidget {
  const _FeedComposer();

  @override
  State<_FeedComposer> createState() => _FeedComposerState();
}

class _FeedComposerState extends State<_FeedComposer> {
  String _text = '', _link = '';
  @override
  Widget build(BuildContext context) {
    return AlertDialog(
      title: Text(CommunitySection.feed.compose),
      content: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          TextField(
            autofocus: true,
            maxLines: 3,
            decoration: const InputDecoration(labelText: 'Post'),
            onChanged: (value) => setState(() => _text = value.trim()),
          ),
          TextField(
            key: const ValueKey('post-link'),
            decoration: const InputDecoration(labelText: 'Link (optional)'),
            keyboardType: TextInputType.url,
            onChanged: (value) => setState(() => _link = value.trim()),
          ),
        ],
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.pop(context),
          child: const Text('Cancel'),
        ),
        TextButton.icon(
          icon: const Icon(Icons.add_photo_alternate_outlined),
          label: const Text('Post a picture'),
          onPressed: _link.isNotEmpty
              ? null
              : () => Navigator.pop(context, {'picture': true, 'body': _text}),
        ),
        FilledButton(
          onPressed: _text.isEmpty
              ? null
              : () => Navigator.pop(context, {
                  'body': _text,
                  if (_link.isNotEmpty) 'link': _link,
                }),
          child: const Text('Publish'),
        ),
      ],
    );
  }
}

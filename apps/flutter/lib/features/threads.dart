import 'dart:async';

import 'package:flutter/material.dart';

import '../bridge.dart';
import '../runtime/context.dart';

class ThreadsScreen extends StatefulWidget {
  const ThreadsScreen({super.key, required this.bridge, required this.onOpen});

  final WaddleBridge bridge;
  final void Function(String jid, String name, bool group, {String? threadId})
  onOpen;

  @override
  State<ThreadsScreen> createState() => _ThreadsScreenState();
}

class _ThreadsScreenState extends State<ThreadsScreen> with RouteAware {
  ModalRoute<void>? _route;
  final _search = TextEditingController();
  final _entries = <(String, String), Map<String, dynamic>>{};
  String _status = 'all', _sort = 'recent';
  String? _channel, _after, _error;
  int _days = 0, _request = 0;
  DateTime? _since;
  Timer? _debounce;
  bool _loading = false;
  final _marking = <(String, String)>{};

  @override
  void initState() {
    super.initState();
    unawaited(_load());
  }

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    final route = ModalRoute.of<void>(context);
    if (route == _route) return;
    waddleRoutes.unsubscribe(this);
    _route = route;
    if (route != null) waddleRoutes.subscribe(this, route);
  }

  @override
  void didPopNext() {
    unawaited(_load());
  }

  @override
  void dispose() {
    waddleRoutes.unsubscribe(this);
    _debounce?.cancel();
    _search.dispose();
    super.dispose();
  }

  Future<void> _load({bool append = false}) async {
    _debounce?.cancel();
    if (append && (_loading || _after == null)) return;
    final request = ++_request;
    final generation = widget.bridge.generation;
    setState(() {
      _loading = true;
      _error = null;
      if (!append) {
        _after = null;
        _entries.clear();
        _since = _days == 0
            ? null
            : DateTime.now().toUtc().subtract(Duration(days: _days));
      }
    });
    try {
      final result = runtimeMap(
        await widget.bridge.invoke('threads.list', {
          'status': _status,
          'sort': _sort,
          'search': _search.text.trim(),
          'channel': ?_channel,
          'active_since': ?_since?.toIso8601String(),
          'after_cursor': ?_after,
          'page_size': 30,
        }),
      );
      if (!mounted ||
          request != _request ||
          generation != widget.bridge.generation) {
        return;
      }
      if (result['entries'] is! List) {
        throw StateError('The server returned an invalid threads page.');
      }
      setState(() {
        for (final entry in runtimeRows(result['entries'])) {
          final jid = entry['channel'], id = entry['thread_id'];
          if (jid is String &&
              jid.isNotEmpty &&
              id is String &&
              id.isNotEmpty) {
            _entries[(jid, id)] = entry;
          }
        }
        final next = result['next_cursor'];
        _after = next is String && next.isNotEmpty && next != _after
            ? next
            : null;
      });
    } catch (error) {
      if (mounted && request == _request) {
        setState(() => _error = WaddleBridge.describeError(error));
      }
    } finally {
      if (mounted && request == _request) setState(() => _loading = false);
    }
  }

  Future<void> _markRead((String, String) key) async {
    if (!_marking.add(key)) return;
    setState(() {});
    try {
      await widget.bridge.invoke('threads.markRead', {
        'jid': key.$1,
        'threadId': key.$2,
      });
      if (mounted) await _load();
    } catch (error) {
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
          SnackBar(content: Text(WaddleBridge.describeError(error))),
        );
      }
    } finally {
      if (mounted) setState(() => _marking.remove(key));
    }
  }

  Widget _filter<T>(
    String label,
    T value,
    Map<T, String> choices,
    ValueChanged<T> choose,
  ) => PopupMenuButton<T>(
    tooltip: label,
    initialValue: value,
    onSelected: (next) {
      choose(next);
      unawaited(_load());
    },
    itemBuilder: (_) => [
      for (final option in choices.entries)
        PopupMenuItem(value: option.key, child: Text(option.value)),
    ],
    child: Chip(
      label: Text(choices[value] ?? label),
      avatar: const Icon(Icons.expand_more, size: 16),
    ),
  );

  @override
  Widget build(BuildContext context) {
    final rooms = {
      for (final row in runtimeRows(widget.bridge.home['rooms']))
        row['jid'] as String: row,
    };
    return Scaffold(
      appBar: AppBar(
        title: const Text('Threads'),
        actions: [
          IconButton(
            tooltip: 'Refresh threads',
            icon: const Icon(Icons.refresh),
            onPressed: () => _load(),
          ),
        ],
      ),
      body: SafeArea(
        child: Column(
          children: [
            Padding(
              padding: const EdgeInsets.fromLTRB(16, 4, 16, 8),
              child: TextField(
                controller: _search,
                decoration: const InputDecoration(
                  labelText: 'Search threads',
                  prefixIcon: Icon(Icons.search),
                ),
                onChanged: (_) {
                  ++_request;
                  _debounce?.cancel();
                  setState(() {
                    _loading = true;
                    _entries.clear();
                    _after = null;
                    _error = null;
                  });
                  _debounce = Timer(const Duration(milliseconds: 250), _load);
                },
                onSubmitted: (_) => _load(),
              ),
            ),
            Padding(
              padding: const EdgeInsets.symmetric(horizontal: 16),
              child: Wrap(
                spacing: 8,
                children: [
                  _filter('Thread status', _status, {
                    'all': 'All threads',
                    'unread': 'Unread',
                    'following': 'Following',
                  }, (v) => _status = v),
                  _filter('Sort threads', _sort, {
                    'recent': 'Recent',
                    'unread': 'Most unread',
                    'replies': 'Most replies',
                  }, (v) => _sort = v),
                  _filter('Active since', _days, {
                    0: 'Any time',
                    1: 'Last day',
                    7: 'Last week',
                    30: 'Last month',
                  }, (v) => _days = v),
                  if (rooms.isNotEmpty)
                    _filter('Filter by room', _channel ?? '', {
                      '': 'All rooms',
                      for (final item in rooms.entries)
                        item.key: '${item.value['name'] ?? item.key}',
                    }, (v) => _channel = v.isEmpty ? null : v),
                ],
              ),
            ),
            if (_loading) const LinearProgressIndicator(),
            Expanded(
              child: _error != null && _entries.isEmpty
                  ? Center(
                      child: TextButton(
                        onPressed: () => _load(),
                        child: Text('$_error · Retry'),
                      ),
                    )
                  : !_loading && _entries.isEmpty
                  ? const Center(child: Text('No matching threads'))
                  : ListView(
                      children: [
                        for (final row in _entries.entries)
                          _entry(row.key, row.value, rooms[row.key.$1]),
                        if (_after != null)
                          TextButton(
                            onPressed: _loading
                                ? null
                                : () => _load(append: true),
                            child: Text(
                              _error != null
                                  ? 'Retry loading more threads'
                                  : 'Load more threads',
                            ),
                          ),
                      ],
                    ),
            ),
          ],
        ),
      ),
    );
  }

  Widget _entry(
    (String, String) key,
    Map<String, dynamic> entry,
    Map<String, dynamic>? room,
  ) {
    final jid = key.$1, thread = key.$2;
    final name = '${room?['name'] ?? jid}';
    final unread = (entry['unread'] as num?)?.toInt() ?? 0;
    final count = (entry['reply_count'] as num?)?.toInt() ?? 0;
    final title = '${entry['thread_title'] ?? entry['preview'] ?? name}';
    return ListTile(
      leading: Badge.count(
        count: unread,
        isLabelVisible: unread > 0,
        child: const Icon(Icons.forum_outlined),
      ),
      title: Text(title, maxLines: 2, overflow: TextOverflow.ellipsis),
      subtitle: Text('$name · $count ${count == 1 ? 'reply' : 'replies'}'),
      onTap: () => widget.onOpen(
        jid,
        name,
        entry['group'] == true || room != null,
        threadId: thread,
      ),
      trailing: unread > 0
          ? IconButton(
              tooltip: 'Mark thread read',
              icon: const Icon(Icons.done_all),
              onPressed: _marking.contains(key) ? null : () => _markRead(key),
            )
          : null,
    );
  }
}

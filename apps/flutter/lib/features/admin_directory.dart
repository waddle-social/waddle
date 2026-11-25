part of 'surfaces.dart';

enum _AdminTable {
  users('Users'),
  spaces('Spaces'),
  rooms('Rooms'),
  members('Members'),
  occupants('Occupants'),
  affiliations('Affiliations');

  const _AdminTable(this.label);
  final String label;
  bool get searchable => this == users || this == spaces || this == rooms;
  String get command => 'admin.$name';

  Object identity(Map<String, dynamic> row) => switch (this) {
    spaces => (row['space_jid'], row['space_node']),
    rooms => row['channel_jid'] as String,
    occupants => (row['real_jid'], row['nick']),
    _ => row['jid'] as String,
  };

  String title(Map<String, dynamic> row) => _text(
    row['name'] ??
        row['nick'] ??
        row['jid'] ??
        row['channel_jid'] ??
        row['space_node'],
  );

  String detail(Map<String, dynamic> row) => switch (this) {
    users => "${row['jid']} · ${row['role'] ?? 'Member'}",
    spaces => "${row['channel_count']} rooms · ${row['member_count']} members",
    rooms => [
      row['channel_type'],
      row['is_public'] == true ? 'Public' : 'Hidden',
      if (row['members_only'] == true) 'Members only',
      "${row['occupant_count']} present",
      "${row['member_count']} members",
    ].join(' · '),
    occupants => [row['real_jid'], row['role'], row['affiliation']].join(' · '),
    members => _text(row['role']),
    affiliations => [
      row['affiliation'],
      if (row['reason'] != null) row['reason'],
    ].join(' · '),
  };
}

bool _adminReady(WaddleBridge bridge) =>
    bridge.home['isCommunityOwner'] == true &&
    bridge.topic('connection')['state'] == 'ready';

class _AdminDirectory extends StatefulWidget {
  const _AdminDirectory({
    super.key,
    required this.bridge,
    required this.table,
    required this.onSelect,
    this.scope = const {},
    this.exclude = const {},
  });
  final WaddleBridge bridge;
  final _AdminTable table;
  final Map<String, dynamic> scope;
  final Set<String> exclude;
  final void Function(Map<String, dynamic>) onSelect;

  @override
  State<_AdminDirectory> createState() => _AdminDirectoryState();
}

class _AdminDirectoryState extends State<_AdminDirectory> {
  final _query = TextEditingController();
  final _rowsById = <Object, Map<String, dynamic>>{};
  final _cursors = <String>{};
  String? _next, _error;
  bool _loading = false, _ready = false;
  int _ticket = 0, _generation = -1;
  Timer? _timer;

  @override
  void initState() {
    super.initState();
    widget.bridge.addListener(_updateAccess);
    _updateAccess();
  }

  void _updateAccess() {
    final ready = _adminReady(widget.bridge);
    if (ready == _ready && _generation == widget.bridge.generation) return;
    _generation = widget.bridge.generation;
    _ready = ready;
    _timer?.cancel();
    _reset();
    if (ready) unawaited(_load());
  }

  void _reset() {
    _ticket++;
    setState(() {
      _rowsById.clear();
      _cursors.clear();
      _next = _error = null;
      _loading = false;
    });
  }

  Future<void> _load({String? cursor}) async {
    if (!_ready || _loading) return;
    final ticket = ++_ticket;
    setState(() {
      _loading = true;
      _error = null;
    });
    try {
      final page = _map(
        await widget.bridge.invokeQuiet(widget.table.command, {
          ...widget.scope,
          if (widget.table.searchable) 'prefix': _query.text.trim(),
          'afterCursor': ?cursor,
        }),
      );
      if (!mounted || ticket != _ticket) return;
      setState(() {
        for (final row in _rows(page['entries'] ?? page['users'])) {
          if (!widget.exclude.contains(row['jid'])) {
            _rowsById[widget.table.identity(row)] = row;
          }
        }
        if (cursor != null) _cursors.add(cursor);
        final next = page['nextCursor'] as String?;
        _next = next == null || next.isEmpty || _cursors.contains(next)
            ? null
            : next;
      });
    } catch (error) {
      if (mounted && ticket == _ticket) {
        setState(() => _error = error.toString());
      }
    } finally {
      if (mounted && ticket == _ticket) setState(() => _loading = false);
    }
  }

  @override
  void dispose() {
    _timer?.cancel();
    widget.bridge.removeListener(_updateAccess);
    _query.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final rows = _rowsById.values.toList(growable: false);
    return Column(
      children: [
        if (widget.table.searchable)
          Padding(
            padding: const EdgeInsets.all(12),
            child: SearchBar(
              controller: _query,
              leading: const Icon(Icons.search),
              hintText: 'Search ${widget.table.label.toLowerCase()}',
              onChanged: (_) {
                _timer?.cancel();
                _reset();
                _timer = Timer(const Duration(milliseconds: 250), _load);
              },
            ),
          ),
        if (_loading || !_ready) const LinearProgressIndicator(),
        if (_error != null)
          MaterialBanner(
            content: Text(_error!),
            actions: [
              TextButton(
                onPressed: () => _load(cursor: _next),
                child: const Text('Retry'),
              ),
            ],
          ),
        Expanded(
          child: ListView.builder(
            itemCount: rows.length + 1,
            itemBuilder: (context, index) {
              if (index < rows.length) {
                final row = rows[index];
                return ListTile(
                  key: ValueKey(widget.table.identity(row)),
                  title: Text(widget.table.title(row)),
                  subtitle: Text(widget.table.detail(row)),
                  onTap: _ready ? () => widget.onSelect(row) : null,
                );
              }
              if (_next != null && _error == null) {
                return TextButton(
                  onPressed: _loading ? null : () => _load(cursor: _next),
                  child: const Text('Load more'),
                );
              }
              if (rows.isEmpty && _ready && !_loading && _error == null) {
                return ListTile(
                  title: Text('No ${widget.table.label.toLowerCase()} found'),
                );
              }
              return const SizedBox.shrink();
            },
          ),
        ),
      ],
    );
  }
}

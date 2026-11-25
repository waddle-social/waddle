part of 'surfaces.dart';

class CommunityAdminScreen extends StatelessWidget {
  const CommunityAdminScreen({super.key, required this.bridge});
  final WaddleBridge bridge;

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: bridge,
    builder: (context, _) {
      if (bridge.home['isCommunityOwner'] != true) {
        return Scaffold(
          appBar: AppBar(title: const Text('Community administration')),
          body: const Center(
            child: Text('Community owner access is required.'),
          ),
        );
      }
      return _AdminNavigator(key: ValueKey(bridge.generation), bridge: bridge);
    },
  );
}

class _AdminNavigator extends StatefulWidget {
  const _AdminNavigator({super.key, required this.bridge});
  final WaddleBridge bridge;
  @override
  State<_AdminNavigator> createState() => _AdminNavigatorState();
}

class _AdminNavigatorState extends State<_AdminNavigator> {
  final _navigator = GlobalKey<NavigatorState>();
  @override
  Widget build(BuildContext context) => NavigatorPopHandler<void>(
    onPopWithResult: (_) => _navigator.currentState?.maybePop(),
    child: Navigator(
      key: _navigator,
      onGenerateRoute: (_) => MaterialPageRoute<void>(
        builder: (_) => _AdminPanel(
          bridge: widget.bridge,
          onClose: () => Navigator.maybePop(context),
        ),
      ),
    ),
  );
}

class _AdminPanel extends StatefulWidget {
  const _AdminPanel({
    required this.bridge,
    this.space,
    this.room,
    this.onClose,
  });
  final WaddleBridge bridge;
  final Map<String, dynamic>? space, room;
  final VoidCallback? onClose;
  @override
  State<_AdminPanel> createState() => _AdminPanelState();
}

class _AdminPanelState extends State<_AdminPanel> {
  late final _tables = widget.room != null
      ? [_AdminTable.occupants, _AdminTable.affiliations]
      : widget.space != null
      ? [_AdminTable.rooms, _AdminTable.members]
      : [_AdminTable.users, _AdminTable.spaces, _AdminTable.rooms];
  late _AdminTable _table = _tables.first;
  late Map<String, dynamic>? _resource = widget.room ?? widget.space;
  int _revision = 0;
  bool _busy = false;
  WaddleBridge get _bridge => widget.bridge;
  late final int _generation = _bridge.generation;
  bool get _current =>
      mounted && _bridge.generation == _generation && _adminReady(_bridge);
  Map<String, dynamic> get _scope => {
    if (widget.space != null) ...{
      'space_jid': widget.space!['space_jid'],
      'space_node': widget.space!['space_node'],
    },
    if (widget.room != null) 'channel_jid': widget.room!['channel_jid'],
  };
  String get _kind => widget.room != null ? 'room' : 'space';

  Future<T?> _push<T>(Widget page) =>
      Navigator.of(context).push<T>(MaterialPageRoute(builder: (_) => page));

  Future<void> _perform(Future<void> Function() operation) async {
    if (_busy || !_current) return;
    setState(() => _busy = true);
    try {
      await operation();
    } catch (error) {
      if (mounted && _current) {
        ScaffoldMessenger.of(
          context,
        ).showSnackBar(SnackBar(content: Text(error.toString())));
      }
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _select(Map<String, dynamic> row) async {
    switch (_table) {
      case _AdminTable.users:
        await _push<void>(
          ProfileScreen(bridge: _bridge, jid: row['jid'] as String),
        );
      case _AdminTable.spaces:
      case _AdminTable.rooms:
        await _push<void>(
          _AdminPanel(
            bridge: _bridge,
            space: _table == _AdminTable.spaces ? row : null,
            room: _table == _AdminTable.rooms ? row : null,
          ),
        );
        if (_current) setState(() => _revision++);
      default:
        await _editRole(row);
    }
  }

  Future<void> _editResource({required bool create}) async {
    final isRoom = create ? _table == _AdminTable.rooms : widget.room != null;
    final fields = await _push<Map<String, dynamic>>(
      _AdminEditor(
        bridge: _bridge,
        room: isRoom,
        initial: create ? null : _resource,
        space: create ? widget.space : null,
      ),
    );
    if (fields == null || !_current) return;
    final command =
        'admin.${create ? 'create' : 'update'}${isRoom ? 'Room' : 'Space'}';
    final result = await _bridge.invokeQuiet(command, {..._scope, ...fields});
    if (_current) {
      setState(() {
        if (!create) _resource = {...?_resource, ..._map(result)};
        _revision++;
      });
    }
  }

  Future<void> _deleteResource() async {
    final name = _text(_resource?['name']);
    final accepted = await _adminConfirm(
      context,
      'Delete $_kind?',
      widget.space != null
          ? 'Delete “$name” and its rooms? This cannot be undone.'
          : 'Delete “$name” for everyone? This cannot be undone.',
    );
    if (accepted == null || !_current) return;
    await _bridge.invokeQuiet(
      widget.space != null ? 'admin.deleteSpace' : 'admin.deleteRoom',
      _scope,
    );
    if (mounted && _current) Navigator.of(context).pop();
  }

  Future<Set<String>> _assignedJids() async {
    final excluded = <String>{}, cursors = <String>{};
    String? after;
    do {
      final page = _map(
        await _bridge.invokeQuiet(_table.command, {
          ..._scope,
          'afterCursor': ?after,
        }),
      );
      if (!_current) throw StateError('The session changed.');
      excluded.addAll(
        _rows(page['entries']).map((row) => row['jid'] as String),
      );
      after = page['nextCursor'] as String?;
      if (after != null && !cursors.add(after)) {
        throw StateError('The directory returned a repeated cursor.');
      }
    } while (after != null && after.isNotEmpty);
    return excluded;
  }

  Future<void> _addMember() async {
    final excluded = await _assignedJids();
    if (!_current) return;
    final user = await _push<Map<String, dynamic>>(
      _AdminPicker(
        bridge: _bridge,
        table: _AdminTable.users,
        exclude: excluded,
      ),
    );
    if (user != null && _current) await _editRole(user, adding: true);
  }

  Future<void> _editRole(
    Map<String, dynamic> row, {
    bool adding = false,
  }) async {
    final space = widget.space != null;
    final occupant = _table == _AdminTable.occupants;
    final jid = _text(occupant ? row['real_jid'] : row['jid']).split('/').first;
    final existing = adding ? null : row[space ? 'role' : 'affiliation'];
    final choice = await showDialog<String>(
      context: context,
      useRootNavigator: false,
      builder: (context) => SimpleDialog(
        title: Text(jid),
        children: [
          for (final role in [
            'owner',
            'admin',
            'member',
            if (!space) 'outcast',
            if (!adding && existing != 'none') 'none',
          ])
            SimpleDialogOption(
              onPressed: role == existing
                  ? null
                  : () => Navigator.pop(context, role),
              child: Text(_roleLabel(role)),
            ),
          if (occupant)
            SimpleDialogOption(
              onPressed: () => Navigator.pop(context, 'kick'),
              child: const Text('Remove from room'),
            ),
          SimpleDialogOption(
            onPressed: () => Navigator.pop(context),
            child: const Text('Cancel'),
          ),
        ],
      ),
    );
    if (!mounted || choice == null || !_current) return;
    String? reason;
    if (choice == 'outcast' || choice == 'none' || choice == 'kick') {
      final message = switch (choice) {
        'outcast' => 'Ban $jid from this room?',
        'kick' when _resource?['members_only'] == true =>
          'Remove $jid from this members-only room and end their membership?',
        'kick' => 'Remove $jid from the room? They can rejoin.',
        _ => 'Remove the role held by $jid?',
      };
      reason = await _adminConfirm(
        context,
        _roleLabel(choice),
        message,
        reason: !space,
      );
      if (reason == null || !_current) return;
    }
    final method = choice == 'kick'
        ? 'admin.kick'
        : space
        ? 'admin.setRole'
        : 'admin.setAffiliation';
    await _bridge.invokeQuiet(method, {
      ..._scope,
      if (choice == 'kick') 'occupant_jid': jid else 'member_jid': jid,
      if (space)
        'role': choice
      else if (choice != 'kick')
        'affiliation': choice,
      'reason': ?reason,
    });
    if (_current) setState(() => _revision++);
  }

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: _bridge,
    builder: (context, _) {
      final enabled = !_busy && _current;
      final create =
          _table == _AdminTable.rooms || _table == _AdminTable.spaces;
      final add =
          _table == _AdminTable.members || _table == _AdminTable.affiliations;
      return Scaffold(
        appBar: AppBar(
          leading: widget.onClose == null
              ? null
              : BackButton(onPressed: widget.onClose),
          title: Text(_text(_resource?['name'], 'Community administration')),
          actions: [
            if (_resource != null)
              PopupMenuButton<String>(
                enabled: enabled,
                onSelected: (value) => _perform(
                  value == 'edit'
                      ? () => _editResource(create: false)
                      : _deleteResource,
                ),
                itemBuilder: (_) => [
                  PopupMenuItem(value: 'edit', child: Text('Edit $_kind')),
                  PopupMenuItem(value: 'delete', child: Text('Delete $_kind')),
                ],
              ),
          ],
        ),
        body: SafeArea(
          child: Column(
            children: [
              Padding(
                padding: const EdgeInsets.all(8),
                child: SegmentedButton<_AdminTable>(
                  segments: [
                    for (final tab in _tables)
                      ButtonSegment(value: tab, label: Text(tab.label)),
                  ],
                  selected: {_table},
                  showSelectedIcon: false,
                  onSelectionChanged: _busy
                      ? null
                      : (selected) => setState(() => _table = selected.single),
                ),
              ),
              if (create || add)
                Align(
                  alignment: AlignmentDirectional.centerEnd,
                  child: TextButton.icon(
                    onPressed: enabled
                        ? () => _perform(
                            create
                                ? () => _editResource(create: true)
                                : _addMember,
                          )
                        : null,
                    icon: const Icon(Icons.add),
                    label: Text(
                      create
                          ? (_table == _AdminTable.spaces
                                ? 'Create space'
                                : 'Create room')
                          : 'Add member',
                    ),
                  ),
                ),
              Expanded(
                child: _AdminDirectory(
                  key: ValueKey((_table, _revision)),
                  bridge: _bridge,
                  table: _table,
                  scope: _scope,
                  onSelect: (row) => _perform(() => _select(row)),
                ),
              ),
            ],
          ),
        ),
      );
    },
  );
}

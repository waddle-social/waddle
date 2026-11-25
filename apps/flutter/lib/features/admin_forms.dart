part of 'surfaces.dart';

String _roleLabel(String role) => switch (role) {
  'owner' => 'Owner',
  'admin' => 'Admin',
  'member' => 'Member',
  'outcast' => 'Ban from room',
  'none' => 'Remove role',
  'kick' => 'Remove from room',
  _ => role,
};

Future<String?> _adminConfirm(
  BuildContext context,
  String title,
  String message, {
  bool reason = false,
}) {
  var note = '';
  return showDialog<String>(
    context: context,
    useRootNavigator: false,
    builder: (context) => AlertDialog(
      title: Text(title),
      content: SingleChildScrollView(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Text(message),
            if (reason)
              TextField(
                decoration: const InputDecoration(
                  labelText: 'Reason (optional)',
                ),
                onChanged: (value) => note = value,
              ),
          ],
        ),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.pop(context),
          child: const Text('Cancel'),
        ),
        FilledButton(
          onPressed: () => Navigator.pop(context, note.trim()),
          child: const Text('Confirm'),
        ),
      ],
    ),
  );
}

class _AdminPicker extends StatelessWidget {
  const _AdminPicker({
    required this.bridge,
    required this.table,
    this.exclude = const {},
  });
  final WaddleBridge bridge;
  final _AdminTable table;
  final Set<String> exclude;
  @override
  Widget build(BuildContext context) => Scaffold(
    appBar: AppBar(
      title: Text(
        table == _AdminTable.users ? 'Choose a user' : 'Choose a space',
      ),
    ),
    body: SafeArea(
      child: _AdminDirectory(
        bridge: bridge,
        table: table,
        exclude: exclude,
        onSelect: (row) => Navigator.pop(context, row),
      ),
    ),
  );
}

class _AdminEditor extends StatefulWidget {
  const _AdminEditor({
    required this.bridge,
    required this.room,
    this.initial,
    this.space,
  });
  final WaddleBridge bridge;
  final bool room;
  final Map<String, dynamic>? initial, space;
  @override
  State<_AdminEditor> createState() => _AdminEditorState();
}

class _AdminEditorState extends State<_AdminEditor> {
  final _form = GlobalKey<FormState>();
  late String _name = _text(widget.initial?['name']);
  late String _description = _text(
    widget.initial?[widget.room ? 'topic' : 'description'],
  );
  late String _iconUrl = _text(widget.initial?['icon_url']);
  late String _kind = _text(widget.initial?['channel_type'], 'text');
  late bool _public = widget.initial?['is_public'] != false;
  late bool _membersOnly = widget.initial?['members_only'] == true;
  late Map<String, dynamic>? _space = widget.space;
  @override
  Widget build(BuildContext context) => Scaffold(
    appBar: AppBar(
      title: Text(
        '${widget.initial == null ? 'Create' : 'Edit'} ${widget.room ? 'room' : 'space'}',
      ),
    ),
    body: SafeArea(
      child: Form(
        key: _form,
        child: ListView(
          padding: const EdgeInsets.all(20),
          children: [
            TextFormField(
              initialValue: _name,
              autofocus: true,
              decoration: const InputDecoration(labelText: 'Name'),
              onChanged: (value) => _name = value,
              validator: (value) =>
                  value == null || value.trim().isEmpty ? 'Enter a name' : null,
            ),
            TextFormField(
              initialValue: _description,
              decoration: const InputDecoration(labelText: 'Description'),
              maxLines: 3,
              onChanged: (value) => _description = value,
            ),
            if (!widget.room)
              TextFormField(
                initialValue: _iconUrl,
                keyboardType: TextInputType.url,
                decoration: const InputDecoration(
                  labelText: 'Icon URL (optional)',
                ),
                onChanged: (value) => _iconUrl = value,
              ),
            if (widget.room) ...[
              DropdownButtonFormField<String>(
                initialValue: _kind,
                decoration: const InputDecoration(labelText: 'Room kind'),
                items: [
                  for (final type in {
                    'text',
                    'announcement',
                    'forum',
                    'group_dm',
                    _kind,
                  })
                    DropdownMenuItem(value: type, child: Text(type)),
                ],
                onChanged: (value) => setState(() => _kind = value!),
              ),
              SwitchListTile(
                title: const Text('Publicly listed'),
                value: _public,
                onChanged: (value) => setState(() => _public = value),
              ),
              SwitchListTile(
                title: const Text('Members only'),
                value: _membersOnly,
                onChanged: (value) => setState(() => _membersOnly = value),
              ),
              if (widget.initial == null)
                ListTile(
                  title: const Text('Space'),
                  subtitle: Text(_text(_space?['name'], 'No space')),
                  enabled: widget.space == null,
                  trailing: _space == null || widget.space != null
                      ? null
                      : IconButton(
                          tooltip: 'Clear space',
                          icon: const Icon(Icons.clear),
                          onPressed: () => setState(() => _space = null),
                        ),
                  onTap: () async {
                    final selected = await Navigator.of(context)
                        .push<Map<String, dynamic>>(
                          MaterialPageRoute(
                            builder: (_) => _AdminPicker(
                              bridge: widget.bridge,
                              table: _AdminTable.spaces,
                            ),
                          ),
                        );
                    if (mounted && selected != null) {
                      setState(() => _space = selected);
                    }
                  },
                ),
            ],
            const SizedBox(height: 16),
            FilledButton(
              onPressed: () {
                if (!_form.currentState!.validate() ||
                    !_adminReady(widget.bridge)) {
                  return;
                }
                Navigator.pop(context, {
                  'name': _name.trim(),
                  if (widget.room) ...{
                    'topic': _description.trim(),
                    'channel_type': _kind,
                    'is_public': _public,
                    'members_only': _membersOnly,
                  } else ...{
                    'description': _description.trim(),
                    'icon_url': _iconUrl.trim(),
                  },
                  if (_space != null) ...{
                    'space_jid': _space!['space_jid'],
                    'space_node': _space!['space_node'],
                  },
                });
              },
              child: const Text('Save'),
            ),
          ],
        ),
      ),
    ),
  );
}

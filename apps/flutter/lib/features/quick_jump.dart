part of '../app.dart';

class _JumpEntry {
  const _JumpEntry(
    this.label,
    this.icon, {
    this.route = '',
    this.row,
    this.group = false,
  });
  final String label, route;
  final Map<String, dynamic>? row;
  final bool group;
  final IconData icon;
}

class _QuickJump extends StatefulWidget {
  const _QuickJump({required this.bridge});
  final WaddleBridge bridge;
  @override
  State<_QuickJump> createState() => _QuickJumpState();
}

class _QuickJumpState extends State<_QuickJump> {
  String _query = '';
  @override
  Widget build(BuildContext context) => Dialog(
    child: ConstrainedBox(
      constraints: const BoxConstraints(maxWidth: 480, maxHeight: 440),
      child: AnimatedBuilder(
        animation: widget.bridge,
        builder: (context, _) {
          final home = widget.bridge.home;
          final people = {
            for (final row in [
              ...records(home['dmPeers']),
              ...records(home['people']),
            ])
              row['jid']: row,
          };
          final results =
              [
                    for (final view in [
                      ..._views,
                      ..._more,
                      (
                        route: 'rooms',
                        label: 'Browse all rooms',
                        icon: Icons.explore_outlined,
                      ),
                      (
                        route: 'settings',
                        label: 'Settings',
                        icon: Icons.settings_outlined,
                      ),
                    ])
                      _JumpEntry(view.label, view.icon, route: view.route),
                    for (final room in records(home['rooms']))
                      _JumpEntry(
                        _entryName(room),
                        _roomIcon(room),
                        row: room,
                        group: true,
                      ),
                    for (final person in people.values)
                      _JumpEntry(
                        _entryName(person),
                        Icons.person_outline,
                        row: person,
                      ),
                  ]
                  .where(
                    (r) => '${r.label} ${r.row?['jid'] ?? ''}'
                        .toLowerCase()
                        .contains(_query.trim().toLowerCase()),
                  )
                  .toList();
          return Column(
            children: [
              Padding(
                padding: const EdgeInsets.all(12),
                child: TextField(
                  autofocus: true,
                  decoration: const InputDecoration(
                    hintText: 'Search rooms, people or pages',
                    prefixIcon: Icon(Icons.search),
                  ),
                  onChanged: (v) => setState(() => _query = v),
                  onSubmitted: (_) {
                    if (results.isNotEmpty) {
                      Navigator.pop(context, results.first);
                    }
                  },
                ),
              ),
              if (results.isEmpty) const _SideNote('No matches.'),
              Expanded(
                child: ListView.builder(
                  itemCount: results.length,
                  itemBuilder: (context, i) => ListTile(
                    leading: Icon(results[i].icon),
                    title: Text(results[i].label),
                    subtitle: results[i].row == null
                        ? null
                        : Text('${results[i].row!['jid']}'),
                    onTap: () => Navigator.pop(context, results[i]),
                  ),
                ),
              ),
            ],
          );
        },
      ),
    ),
  );
}

part of '../app.dart';

class _RoomDirectory extends StatefulWidget {
  const _RoomDirectory({required this.bridge, required this.onOpen});
  final WaddleBridge bridge;
  final _OpenEntry onOpen;
  @override
  State<_RoomDirectory> createState() => _RoomDirectoryState();
}

class _RoomDirectoryState extends State<_RoomDirectory> {
  String _query = '';
  Future<void> _create() async {
    final args = await showDialog<Map<String, dynamic>>(
      context: context,
      useRootNavigator: false,
      builder: (_) => const _CreateRoomDialog(),
    );
    if (args == null || !mounted) return;
    await perform(context, () async {
      final created = await widget.bridge.invoke('room.create', args);
      if (mounted && created is Map && created['jid'] is String) {
        widget.onOpen({'jid': created['jid'], 'name': args['name']}, true);
      }
    });
  }

  @override
  Widget build(BuildContext context) => AnimatedBuilder(
    animation: widget.bridge,
    builder: (context, _) {
      final rows = records(widget.bridge.home['rooms'])
          .where(
            (r) =>
                r['groupDm'] != true &&
                _entryName(r).toLowerCase().contains(_query.toLowerCase()),
          )
          .toList();
      return Scaffold(
        appBar: AppBar(
          title: const Text('Rooms'),
          actions: [
            if (widget.bridge.home['canCreate'] == true)
              IconButton(
                tooltip: 'New room',
                onPressed: _create,
                icon: const Icon(Icons.add),
              ),
          ],
        ),
        body: Column(
          children: [
            Padding(
              padding: const EdgeInsets.all(16),
              child: TextField(
                decoration: const InputDecoration(
                  hintText: 'Find a room',
                  prefixIcon: Icon(Icons.search),
                ),
                onChanged: (v) => setState(() => _query = v),
              ),
            ),
            if (rows.isEmpty)
              _SideNote(
                _query.isEmpty ? 'No rooms yet.' : 'No matching rooms.',
              ),
            Expanded(
              child: ListView.builder(
                itemCount: rows.length,
                itemBuilder: (context, i) => ListTile(
                  leading: Icon(_roomIcon(rows[i])),
                  title: Text(_entryName(rows[i])),
                  subtitle: rows[i]['description'] == null
                      ? null
                      : Text('${rows[i]['description']}'),
                  onTap: () => widget.onOpen(rows[i], true),
                ),
              ),
            ),
          ],
        ),
      );
    },
  );
}

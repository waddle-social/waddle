part of 'trip_plan.dart';

class TripPlansScreen extends StatefulWidget {
  const TripPlansScreen({super.key, required this.bridge});
  final WaddleBridge bridge;

  @override
  State<TripPlansScreen> createState() => _TripPlansState();
}

class _TripPlansState extends State<TripPlansScreen> {
  bool _creating = false;

  Future<void> _open(String room) => Navigator.of(context).push(
    MaterialPageRoute(
      builder: (_) => TripPlanScreen(bridge: widget.bridge, room: room),
    ),
  );

  Future<void> _create() async {
    var name = '';
    var private = true;
    final values = await showDialog<(String, bool)>(
      context: context,
      builder: (context) => StatefulBuilder(
        builder: (context, update) => AlertDialog(
          title: const Text('New trip'),
          content: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              TextField(
                autofocus: true,
                maxLength: 200,
                decoration: const InputDecoration(labelText: 'Trip name'),
                onChanged: (value) => update(() => name = value.trim()),
              ),
              SwitchListTile(
                title: const Text('Private room'),
                subtitle: const Text('Only members can join.'),
                value: private,
                onChanged: (value) => update(() => private = value),
              ),
            ],
          ),
          actions: [
            TextButton(
              onPressed: () => Navigator.pop(context),
              child: const Text('Cancel'),
            ),
            FilledButton(
              onPressed: name.isEmpty
                  ? null
                  : () => Navigator.pop(context, (name, private)),
              child: const Text('Create'),
            ),
          ],
        ),
      ),
    );
    if (values == null || !mounted || _creating) return;
    setState(() => _creating = true);
    try {
      final room = await widget.bridge.invokeQuiet('planning.createRoom', {
        'name': values.$1,
        'private': values.$2,
      });
      if (mounted && room is String) await _open(room);
    } catch (error) {
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
          SnackBar(content: Text(WaddleBridge.describeError(error))),
        );
      }
    } finally {
      if (mounted) setState(() => _creating = false);
    }
  }

  @override
  Widget build(BuildContext context) => Scaffold(
    appBar: AppBar(
      title: const Text('Trips'),
      actions: [
        IconButton(
          onPressed: _creating ? null : _create,
          tooltip: 'New trip',
          icon: const Icon(Icons.add),
        ),
      ],
    ),
    body: ListenableBuilder(
      listenable: widget.bridge,
      builder: (context, _) {
        final rooms = (widget.bridge.home['rooms'] as List? ?? const [])
            .whereType<Map>()
            .where((room) => '${room['jid']}'.startsWith('event-'))
            .toList();
        if (_creating) return const Center(child: CircularProgressIndicator());
        if (rooms.isEmpty) {
          return const Center(child: Text('No trip rooms yet'));
        }
        return ListView(
          children: [
            for (final row in rooms)
              ListTile(
                leading: const Icon(Icons.map_outlined),
                title: Text('${row['name'] ?? row['jid']}'),
                subtitle: Text('${row['jid']}'),
                onTap: () => _open(row['jid'] as String),
              ),
          ],
        );
      },
    ),
  );
}

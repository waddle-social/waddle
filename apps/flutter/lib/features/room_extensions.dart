import 'package:flutter/material.dart';

import '../bridge.dart';

typedef ExtensionForm =
    Future<void> Function(dynamic response, Map<String, dynamic> command);

class RoomExtensionsScreen extends StatefulWidget {
  const RoomExtensionsScreen({
    super.key,
    required this.bridge,
    required this.room,
    required this.showForm,
    this.route,
  });
  final WaddleBridge bridge;
  final String room;
  final ExtensionForm showForm;
  final Map<String, dynamic>? route;
  @override
  State<RoomExtensionsScreen> createState() => _RoomExtensionsState();
}

class _RoomExtensionsState extends State<RoomExtensionsScreen> {
  late Future<List<Map<String, dynamic>>> _rows = _load();
  bool _busy = false;

  Future<List<Map<String, dynamic>>> _load() async {
    final items = await widget.bridge.invokeQuiet(
      widget.route == null ? 'extensions.list' : 'extensions.items',
      {
        'jid': widget.room,
        if (widget.route case final route?) 'route': route['key'],
      },
    );
    return List<Map<String, dynamic>>.from(items as List);
  }

  void _reload() => setState(() => _rows = _load());

  Future<void> _launch(Map action) async {
    if (_busy) return;
    setState(() => _busy = true);
    try {
      final result = await widget.bridge.invokeQuiet('extensions.invoke', {
        'jid': widget.room,
        'route': widget.route!['key'],
        'action': action['key'],
      });
      if (!mounted) return;
      await widget.showForm(
        result['response'],
        Map<String, dynamic>.from(result['command'] as Map),
      );
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

  Widget _item(Map<String, dynamic> row) {
    final link = row['link'];
    final url = link is Map ? Uri.tryParse('${link['href']}') : null;
    return Card(
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          mainAxisSize: MainAxisSize.min,
          children: [
            Text(
              '${row['title'] ?? row['id'] ?? 'Item'}',
              style: Theme.of(context).textTheme.titleMedium,
            ),
            if (row['subtitle'] case final String subtitle) Text(subtitle),
            if (row['description'] case final String description)
              SelectableText(description),
            for (final field in (row['fields'] as List? ?? []).whereType<Map>())
              Text('${field['label'] ?? field['name']} · ${field['value']}'),
            if (url != null &&
                {'https', 'http'}.contains(url.scheme) &&
                url.host.isNotEmpty &&
                url.userInfo.isEmpty)
              TextButton(
                onPressed: () =>
                    widget.bridge.invoke('link.open', {'url': url.toString()}),
                child: Text(url.toString()),
              ),
            Wrap(
              spacing: 6,
              children: [
                for (final option
                    in (row['options'] as List? ?? []).whereType<Map>())
                  Chip(label: Text('${option['label']}')),
                for (final action
                    in (row['actions'] as List? ?? []).whereType<Map>())
                  OutlinedButton(
                    onPressed: _busy ? null : () => _launch(action),
                    child: Text('${action['label']}'),
                  ),
              ],
            ),
          ],
        ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) => Scaffold(
    appBar: AppBar(
      title: Text('${widget.route?['label'] ?? 'Room extensions'}'),
      actions: [
        IconButton(
          onPressed: _busy ? null : _reload,
          icon: const Icon(Icons.refresh),
          tooltip: 'Refresh',
        ),
      ],
    ),
    body: SafeArea(
      child: FutureBuilder<List<Map<String, dynamic>>>(
        future: _rows,
        builder: (context, snapshot) {
          if (snapshot.connectionState != ConnectionState.done) {
            return const Center(child: CircularProgressIndicator());
          }
          if (snapshot.hasError) {
            return Center(
              child: Text(WaddleBridge.describeError(snapshot.error!)),
            );
          }
          final rows = snapshot.requireData;
          if (rows.isEmpty) {
            return Center(
              child: Text(
                widget.route == null
                    ? 'No room extensions available'
                    : 'No items in this extension',
              ),
            );
          }
          if (widget.route == null) {
            return ListView(
              children: [
                for (final route in rows)
                  ListTile(
                    leading: Icon(
                      route['surface'] == 'gallery'
                          ? Icons.grid_view
                          : Icons.list,
                    ),
                    title: Text('${route['label']}'),
                    onTap: () => Navigator.of(context).push(
                      MaterialPageRoute<void>(
                        builder: (_) => RoomExtensionsScreen(
                          bridge: widget.bridge,
                          room: widget.room,
                          route: route,
                          showForm: widget.showForm,
                        ),
                      ),
                    ),
                  ),
              ],
            );
          }
          if (widget.route!['surface'] == 'gallery') {
            return LayoutBuilder(
              builder: (context, space) {
                final columns = (space.maxWidth ~/ 300).clamp(1, 4);
                return ListView.builder(
                  itemCount: (rows.length + columns - 1) ~/ columns,
                  itemBuilder: (_, index) => Row(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      for (var column = 0; column < columns; column++)
                        Expanded(
                          child: index * columns + column < rows.length
                              ? _item(rows[index * columns + column])
                              : const SizedBox(),
                        ),
                    ],
                  ),
                );
              },
            );
          }
          return ListView.builder(
            itemCount: rows.length,
            itemBuilder: (_, i) => _item(rows[i]),
          );
        },
      ),
    ),
  );
}

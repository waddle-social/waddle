part of 'surfaces_runtime.dart';

extension _RoomExtensions on SurfaceRuntime {
  Future<dynamic> _extensionRoute(
    String method,
    Map<String, dynamic> args,
  ) async {
    final room = _required(args, 'jid');
    if (method == 'extensions.list') {
      final revision = ++_routeListRevision;
      final rows = runtimeRows(
        await _rpc('discover_extension_routes', {'user_jid': _self}),
      );
      if (revision != _routeListRevision) {
        throw StateError('The extension list was refreshed.');
      }
      _routePages.clear();
      _roomRoutes.clear();
      for (final row in rows) {
        if (row['scope'] != 'channel' ||
            !{'gallery', 'list'}.contains(row['surface'])) {
          continue;
        }
        final key = jsonEncode([
          row['serviceJid'],
          row['pluginId'],
          row['routeId'],
        ]);
        _roomRoutes[key] = row;
      }
      return [
        for (final row in _roomRoutes.entries)
          {
            'key': row.key,
            'label': row.value['label'],
            'surface': row.value['surface'],
          },
      ];
    }
    final key = _required(args, 'route');
    final route = _roomRoutes[key];
    if (route == null) throw StateError('Open the room extensions again.');
    final pageKey = (room, key);
    if (method == 'extensions.items') {
      final handles = <String, Map<String, dynamic>>{};
      _routePages.remove(pageKey);
      if (_routePages.length >= 32) _routePages.remove(_routePages.keys.first);
      _routePages[pageKey] = handles;
      final items = runtimeRows(
        await _rpc('fetch_extension_route_items', {
          'room_jid': room,
          'route': route,
        }),
      );
      if (!identical(_routePages[pageKey], handles)) {
        throw StateError('The extension was refreshed.');
      }
      final rows = <Map<String, dynamic>>[];
      for (final item in items) {
        final actions = <Map<String, dynamic>>[];
        for (final raw in runtimeRows(item['actions'])) {
          final launch = runtimeMap(raw['launch']);
          if (launch['pluginId'] != route['pluginId'] ||
              launch['roomJid'] != null && launch['roomJid'] != room) {
            continue;
          }
          final id = jsonEncode([item['id'], launch['id']]);
          handles[id] = launch;
          actions.add({'key': id, 'label': launch['label']});
        }
        rows.add({...item, 'actions': actions});
      }
      return rows;
    }
    final launch = _routePages[pageKey]?.remove(_required(args, 'action'));
    if (launch == null) {
      throw StateError('Refresh the extension to use this action.');
    }
    final expiry = DateTime.tryParse('${launch['expiresAt']}');
    if (expiry == null || !expiry.isAfter(DateTime.now())) {
      throw StateError('This action has expired. Refresh the extension.');
    }
    final fields = <String, String>{
      'FORM_TYPE': 'urn:waddle:extension:1',
      'waddle#op': 'invoke',
      'plugin': _required(launch, 'pluginId'),
      'action': _required(launch, 'actionId'),
      'waddle-id': _required(launch, 'waddleId'),
      'waddle#waddle_id': _required(launch, 'waddleId'),
      'launch-id': _required(launch, 'id'),
      'waddle#launch_id': _required(launch, 'id'),
      'launch-token': _required(launch, 'launchToken'),
      'waddle#launch_token': _required(launch, 'launchToken'),
      'expires-at': _required(launch, 'expiresAt'),
      'waddle#expires_at': _required(launch, 'expiresAt'),
      if (launch['sourceStanzaId'] case final String stanza) ...{
        'source-stanza-id': stanza,
        'waddle#message_stanza_id': stanza,
      },
    };
    final service = _required(route, 'serviceJid'),
        node = _required(launch, 'commandNode');
    final result = await _rpc('submit_extension_command_form', {
      'service_jid': service,
      'node': node,
      'room_jid': launch['roomJid'],
      'session_id': null,
      'action': 'execute',
      'fields': [
        for (final field in fields.entries)
          {
            'name': field.key,
            'values': [field.value],
          },
      ],
    });
    return {
      'command': {'serviceJid': service, 'node': node},
      'response': _commandResult(result, service, node),
    };
  }
}

part of 'session_runtime.dart';

extension _SessionAdmin on SessionRuntime {
  Set<String> get _blockedRoomJoins =>
      (_persistent['adminDeletedRooms'] as List? ?? [])
          .whereType<String>()
          .toSet();

  Future<dynamic> _deleteAdminResource(
    RustTransport current,
    int captured,
    String method,
    Map<String, dynamic> args,
  ) async {
    final rooms = <String>{};
    if (method == 'admin_channels_delete') {
      rooms.add(bareJid(args['channel_jid'] as String));
    } else {
      // A service can host multiple space nodes. Only the scoped list identifies
      // this space's rooms, including rooms absent from the user's discovery.
      String? cursor;
      final seen = <String>{};
      do {
        final page = runtimeMap(
          await _invokeTransport(current, captured, 'admin_channels_list', {
            'args': {
              'space_jid': args['space_jid'],
              'space_node': args['space_node'],
              'page_size': 50,
              'after_cursor': cursor,
            },
          }),
        );
        if (page['entries'] is! List)
          throw StateError('Could not list the rooms in this space.');
        for (final row in runtimeRows(page['entries'])) {
          rooms.add(bareJid(row['channel_jid'] as String));
        }
        cursor = page['next_cursor'] as String?;
        if (cursor != null && !seen.add(cursor))
          throw StateError('The room directory returned a repeated cursor.');
      } while (cursor != null && cursor.isNotEmpty);
    }
    final result = await _invokeTransport(current, captured, method, args);
    if (result == false) throw StateError('The server refused the deletion.');
    // A MUC join can create a missing room. Discovery re-enables surviving
    // rooms that the server unlinked rather than destroyed with the space.
    _persistent['adminDeletedRooms'] = {
      ..._blockedRoomJoins,
      ...rooms,
    }.toList();
    _joined.removeAll(rooms);
    _persistent['joinedRooms'] = {
      ...(_persistent['joinedRooms'] as List? ?? []).whereType<String>(),
      ..._joined,
    }.difference(rooms).toList();
    _channels.removeWhere(
      (row) => rooms.contains(bareJid('${row['room_jid']}')),
    );
    _emitHome();
    await persist();
    requireGeneration(this, captured);
    return result;
  }
}

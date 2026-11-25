part of 'surfaces_runtime.dart';

extension _AdminRuntime on SurfaceRuntime {
  Future<dynamic> _admin(String method, Map<String, dynamic> args) async {
    final paging = {
      'page_size': 50,
      'after_cursor': _optional(args['afterCursor']),
    };
    final search = {'prefix': _optional(args['prefix']), ...paging};
    Map<String, dynamic> space() => {
      'space_jid': _required(args, 'space_jid'),
      'space_node': _required(args, 'space_node'),
    };
    Map<String, dynamic> room() => {
      'channel_jid': _required(args, 'channel_jid'),
    };
    final (
      String verb,
      Map<String, dynamic> fields,
      bool record,
    ) = switch (method) {
      'admin.users' => ('admin_users_list', search, false),
      'admin.spaces' => ('admin_spaces_list', search, true),
      'admin.rooms' => (
        'admin_channels_list',
        {if (args['space_jid'] != null) ...space(), ...search},
        true,
      ),
      'admin.members' => (
        'admin_spaces_members',
        {...space(), ...paging},
        true,
      ),
      'admin.occupants' => (
        'admin_channels_occupants',
        {...room(), ...paging},
        true,
      ),
      'admin.affiliations' => (
        'admin_channels_affiliations',
        {...room(), ...paging},
        true,
      ),
      'admin.createSpace' || 'admin.updateSpace' => (
        method == 'admin.createSpace'
            ? 'admin_spaces_create'
            : 'admin_spaces_update',
        {
          if (method == 'admin.updateSpace') ...space(),
          'name': _required(args, 'name'),
          'description': _optional(args['description']),
          'icon_url': _optional(args['icon_url']),
        },
        true,
      ),
      'admin.createRoom' || 'admin.updateRoom' => (
        method == 'admin.createRoom'
            ? 'admin_channels_create'
            : 'admin_channels_update',
        {
          if (method == 'admin.updateRoom')
            ...room()
          else if (args['space_jid'] != null)
            ...space(),
          'name': _required(args, 'name'),
          'topic': _optional(args['topic']),
          'channel_type': _required(args, 'channel_type'),
          'is_public': args['is_public'] as bool,
          'members_only': args['members_only'] as bool,
        },
        true,
      ),
      'admin.deleteSpace' => ('admin_spaces_delete', space(), false),
      'admin.deleteRoom' => ('admin_channels_delete', room(), false),
      'admin.setRole' => (
        'admin_spaces_set_role',
        {
          ...space(),
          'member_jid': _required(args, 'member_jid'),
          'role': _required(args, 'role'),
        },
        false,
      ),
      'admin.setAffiliation' => (
        'admin_channels_set_affiliation',
        {
          ...room(),
          'member_jid': _required(args, 'member_jid'),
          'affiliation': _required(args, 'affiliation'),
          'reason': _optional(args['reason']),
        },
        false,
      ),
      'admin.kick' => (
        'admin_channels_kick',
        {
          ...room(),
          'occupant_jid': _required(args, 'occupant_jid'),
          'reason': _optional(args['reason']),
        },
        false,
      ),
      _ => throw UnsupportedError(method),
    };
    final result = await _rpc(verb, record ? {'args': fields} : fields);
    if (fields.containsKey('page_size')) {
      final page = runtimeMap(result);
      if (page['entries'] is! List) {
        throw StateError('Invalid community directory response.');
      }
      final rows = runtimeRows(page['entries']);
      return {
        if (method == 'admin.users')
          'users': [
            for (final row in rows)
              {
                'jid': row['jid'],
                'name': row['display_name'] ?? row['jid'],
                'role': row['has_owner_hat'] == true ? 'Owner' : 'Member',
              },
          ]
        else
          'entries': rows,
        'nextCursor': page['next_cursor'],
      };
    }
    if (const {
      'admin.createSpace',
      'admin.updateSpace',
      'admin.deleteSpace',
      'admin.createRoom',
      'admin.updateRoom',
      'admin.deleteRoom',
    }.contains(method)) {
      unawaited(_refreshHome().catchError((Object _) {}));
    }
    return result;
  }
}

part of 'surfaces_runtime.dart';

extension on SurfaceRuntime {
  Future<dynamic> _community(String method, Map<String, dynamic> args) async {
    final jid = _self;
    final domain = jid.split('@').last;
    final service = 'community.$domain';
    switch (method) {
      case 'community.feed':
        return _rpc('feed_items', {'spaces_jid': service, 'max_items': 100});
      case 'community.post':
        return _rpc('feed_publish', {
          'spaces_jid': service,
          'entry': {'body': _required(args, 'body'), 'author': jid},
        });
      case 'community.stories':
        final items = runtimeRows(
          await _rpc('stories_items', {
            'community_jid': service,
            'max_items': 100,
          }),
        );
        final reads = runtimeMap(await _rpc('story_reads_fetch'));
        final seen = runtimeRows(
          reads['entries'],
        ).map((read) => read['id']).toSet();
        return items
            .where((item) {
              final expires = DateTime.tryParse('${item['expires']}');
              return expires != null && expires.isAfter(DateTime.now());
            })
            .map((item) => {...item, 'read': seen.contains(item['id'])})
            .toList();
      case 'community.readStory':
        final id = _required(args, 'id');
        final pending = _storyReadWrite.then((_) async {
          final state = runtimeMap(await _rpc('story_reads_fetch'));
          final rows = runtimeRows(state['entries']);
          if (rows.any((row) => row['id'] == id)) return;
          await _rpc('story_reads_publish', {
            'input': {
              'entries': [
                ...rows,
                {'id': id, 'at': DateTime.now().toUtc().toIso8601String()},
              ],
            },
          });
        });
        _storyReadWrite = pending.catchError((Object _) {});
        return pending;
      case 'community.publishStory':
        final file = await _platform('file.pickStoryMedia');
        if (file == null) return null;
        return _rpc('stories_publish', {
          'community_jid': service,
          'input': {
            'author': jid,
            'body': _optional(args['body']),
            'media_url': file['url'],
            'media_type': file['media_type'],
          },
        });
      case 'community.events':
        final raw = runtimeRows(
          await _rpc('xcal_items', {
            'community_jid': service,
            'max_items': 200,
          }),
        );
        final responses = <String, Map<String, dynamic>>{};
        for (final row in raw) {
          final uid = row['uid'];
          if (uid is! String) continue;
          for (final person in runtimeRows(row['attendees'])) {
            final uri = Uri.tryParse('${person['uri']}');
            if (uri?.scheme != 'xmpp') continue;
            final who = uri!.path;
            if (row['id'] != uid &&
                row['id'] != '$uid-rsvp-${who.split('@').first}') {
              continue;
            }
            (responses[uid] ??= {})[who] = person['partstat'];
          }
        }
        return raw
            .where(
              (row) => row['id'] == row['uid'] || row['recurrence_id'] != null,
            )
            .map((row) => {...row, 'responses': responses[row['uid']] ?? {}})
            .toList();
      case 'community.createEvent':
        final starts = args['starts'];
        if (starts is! int) throw ArgumentError('Choose a start time.');
        final repeat = args['repeat'];
        if (repeat != null &&
            !{'DAILY', 'WEEKLY', 'MONTHLY', 'YEARLY'}.contains(repeat)) {
          throw ArgumentError('Unknown repeat interval.');
        }
        return _rpc('xcal_publish', {
          'community_jid': service,
          'input': {
            'summary': _required(args, 'summary'),
            'description': _optional(args['description']),
            'organizer': 'xmpp:$jid',
            'dtstart': {'kind': 'date-time', 'ms': starts},
            if (repeat != null) 'rrule': {'freq': repeat},
          },
        });
      case 'community.rsvp':
        return _rpc('xcal_rsvp', {
          'community_jid': service,
          'master_uid': _required(args, 'id'),
          'self_localpart': jid.split('@').first,
          'self_jid': jid,
          'partstat': _required(args, 'response'),
        });
      case 'community.cancelEvent':
        return _rpc('xcal_retract', {
          'community_jid': service,
          'item_id': _required(args, 'id'),
        });
      default:
        throw UnsupportedError('Unknown community action.');
    }
  }
}

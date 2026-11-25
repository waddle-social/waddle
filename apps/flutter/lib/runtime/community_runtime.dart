part of 'surfaces_runtime.dart';

extension on SurfaceRuntime {
  Future<dynamic> _community(String method, Map<String, dynamic> args) async {
    final jid = _self;
    final domain = jid.split('@').last;
    final service = 'community.$domain';
    switch (method) {
      case 'community.feed':
        final posts = runtimeRows(
          await _rpc('feed_items', {'spaces_jid': service, 'max_items': 100}),
        );
        DateTime published(Map<String, dynamic> row) =>
            DateTime.tryParse('${row['published']}') ??
            DateTime.fromMillisecondsSinceEpoch(0);
        return posts
          ..sort((left, right) => published(right).compareTo(published(left)));
      case 'community.post':
        final address = _optional(args['link']);
        final link = address == null
            ? null
            : Uri.tryParse(
                address.contains('://') ? address : 'https://$address',
              );
        if (address != null &&
            (link == null ||
                !{'https', 'http'}.contains(link.scheme) ||
                link.host.isEmpty ||
                link.userInfo.isNotEmpty)) {
          throw const FormatException('Enter a valid web link.');
        }
        return _rpc('feed_publish', {
          'spaces_jid': service,
          'entry': {
            'body': _required(args, 'body'),
            'author': jid,
            if (link != null) 'link': link.toString(),
          },
        });
      case 'community.stories':
        final authors = await _rpc('story_authors') as List;
        final items = <Map<String, dynamic>>[];
        // Keep a large roster from putting hundreds of IQs in flight at once.
        final owners = authors.whereType<String>().toSet().toList();
        for (var i = 0; i < owners.length; i += 8) {
          final pages = await Future.wait(
            owners.skip(i).take(8).map((author) async {
              final rows = runtimeRows(
                await _rpc('fetch_stories', {
                  'author_jid': author,
                  'max_items': 100,
                }),
              );
              return rows.map((row) => {...row, 'author': author});
            }),
          );
          items.addAll(pages.expand((rows) => rows));
        }
        items.sort(
          (a, b) =>
              (DateTime.tryParse('${b['posted']}')?.millisecondsSinceEpoch ?? 0)
                  .compareTo(
                    DateTime.tryParse(
                          '${a['posted']}',
                        )?.millisecondsSinceEpoch ??
                        0,
                  ),
        );
        final reads = runtimeMap(await _rpc('story_reads_fetch'));
        final seen = runtimeRows(
          reads['entries'],
        ).map((read) => read['id']).toSet();
        return items
            .where((item) {
              final expires = DateTime.tryParse('${item['expires']}');
              return item['expires'] == null ||
                  (expires != null && expires.isAfter(DateTime.now()));
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
        final published = runtimeMap(
          await _rpc('publish_story', {
            'input': {
              'body': _optional(args['body']),
              'media_url': file['url'],
              'media_type': file['media_type'],
            },
          }),
        );
        if (published['id'] is String) {
          await _community('community.readStory', {'id': published['id']});
        }
        return published;
      case 'community.storyReactions':
        return _rpc('fetch_story_reactions', {
          'author_jid': _required(args, 'author'),
          'story_id': _required(args, 'id'),
        });
      case 'community.setStoryReactions':
        return _rpc('set_story_reactions', {
          'author_jid': _required(args, 'author'),
          'story_id': _required(args, 'id'),
          'emojis': List<String>.from(args['emojis'] as List),
        });
      case 'community.events':
        final raw = runtimeRows(
          await _rpc('xcal_items', {
            'community_jid': service,
            'max_items': null,
          }),
        );
        final masters = <Map<String, dynamic>>[];
        final overrides = <String, List<Map<String, dynamic>>>{};
        final responses = <String, Map<String, dynamic>>{};
        for (final row in raw) {
          final uid = row['uid'];
          if (uid is! String) continue;
          if (row['recurrence_id'] != null) {
            (overrides[uid] ??= []).add(row);
            continue;
          }
          final attendees = runtimeRows(row['attendees']);
          final isReply =
              attendees.length == 1 &&
              (row['summary'] ?? '') == '' &&
              row['id'] != uid;
          if (!isReply) masters.add(row);
          for (final person in attendees) {
            final uri = Uri.tryParse('${person['uri']}');
            if (uri?.scheme != 'xmpp') continue;
            final who = uri!.path;
            if (isReply && row['id'] != '$uid-rsvp-${who.split('@').first}') {
              continue;
            }
            final target = (responses[uid] ??= {});
            if (isReply) {
              target[who] = person['partstat'];
            } else {
              target.putIfAbsent(who, () => person['partstat']);
            }
          }
        }
        return [
          for (final row in masters)
            {
              ...row,
              'overrides': overrides[row['uid']] ?? [],
              'responses': responses[row['uid']] ?? {},
            },
        ];
      case 'community.createEvent':
        final input = runtimeMap(args['event']);
        if (input['dtstart'] is! Map) {
          throw ArgumentError('Choose a start date.');
        }
        return _rpc('xcal_publish', {
          'community_jid': service,
          'input': {...input, 'organizer': 'xmpp:$jid'},
        });
      case 'community.editEvent':
        return _rpc('edit_calendar_event', {
          'community_jid': service,
          'item_id': _required(args, 'id'),
          'edit': {
            'occurrence': args['occurrence'],
            'event': args['event'],
            'fields': args['fields'],
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

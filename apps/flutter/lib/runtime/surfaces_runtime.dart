import 'dart:async';
import 'dart:convert';
import 'dart:typed_data';

import 'package:http/http.dart' as http;

import 'context.dart';

/// Presentation commands are translated into the shared Rust client's typed
/// verbs. Dart retains only account-fenced caches and UI projections.
class SurfaceRuntime {
  SurfaceRuntime(this.context, {http.Client? httpClient})
    : _http = httpClient ?? http.Client();
  final RuntimeContext context;
  final http.Client _http;
  Map<String, dynamic> _profile = {};
  bool _profileLoaded = false, _disposed = false;
  final _avatars = <String, Map<String, Uint8List>>{};
  final _avatarCurrent = <String, String>{};
  final _forms = <String, Set<String>>{};
  List<Map<String, dynamic>>? _commands;
  List<Map<String, dynamic>> _packs = [];
  static final _commandOwner = Object();
  int get _owner => Zone.current[_commandOwner] as int? ?? context.generation;

  Future<dynamic> _rpc(
    String method, [
    Map<String, dynamic> args = const {},
  ]) async {
    final captured = _owner;
    requireGeneration(context, captured);
    final value = await context.rpc(method, args);
    requireGeneration(context, captured);
    if (_disposed) {
      throw StateError('This controller is closed.');
    }
    if (value == false) {
      throw StateError('The server refused the action.');
    }
    return value;
  }

  String _required(Map<String, dynamic> args, String key) {
    final value = args[key];
    if (value is! String || value.trim().isEmpty) {
      throw ArgumentError('Missing $key');
    }
    return value.trim();
  }

  String? _optional(dynamic value) =>
      value is String && value.trim().isNotEmpty ? value.trim() : null;
  String get _self => context.jid ?? (throw StateError('Sign in first.'));
  String get _nick =>
      '${context.session['xmppLocalpart'] ?? _self.split('@').first}';
  void _emitProfile() => context.emit('profile', Map.of(_profile));

  Future<dynamic> invoke(String method, Map<String, dynamic> args) => runZoned(
    () => _execute(method, args),
    zoneValues: {_commandOwner: context.generation},
  );

  Future<dynamic> _platform(
    String method, [
    Map<String, dynamic> args = const {},
  ]) async {
    final captured = _owner;
    requireGeneration(context, captured);
    final value = await context.platform(method, args);
    requireGeneration(context, captured);
    return value;
  }

  Future<void> _refreshHome() async {
    final captured = _owner;
    requireGeneration(context, captured);
    await context.refreshHome();
    requireGeneration(context, captured);
  }

  Future<dynamic> _execute(String method, Map<String, dynamic> args) async {
    if (_disposed) {
      throw StateError('This controller is closed.');
    }
    switch (method) {
      case 'directory.search':
        final result = await _rpc('search_users', {
          'query': args['query'] ?? '',
        });
        if (result is! List) {
          throw StateError('The directory returned an invalid result.');
        }
        return runtimeRows(result)
            .map(
              (entry) => {
                'jid': entry['jid'],
                'name':
                    entry['display_name'] ??
                    entry['username'] ??
                    '${entry['jid']}'.split('@').first,
                'username': entry['username'],
              },
            )
            .toList();
      case 'admin.users':
        final page = runtimeMap(
          await _rpc('admin_users_list', {
            'prefix': _optional(args['prefix']),
            'page_size': 50,
            'after_cursor': _optional(args['afterCursor']),
          }),
        );
        if (page['entries'] is! List) {
          throw StateError('The user list could not be loaded.');
        }
        return {
          'users': runtimeRows(page['entries'])
              .map(
                (entry) => {
                  'jid': entry['jid'],
                  'username': '${entry['jid']}'.split('@').first,
                  'name': entry['display_name'],
                  'role': entry['has_owner_hat'] == true ? 'Owner' : 'Member',
                },
              )
              .toList(),
          'nextCursor': page['next_cursor'],
        };
      case 'profile.load':
        return _loadProfile(_optional(args['jid']));
      case 'profile.avatarBytes':
        return _avatar(bareJid(_required(args, 'jid')));
      case 'profile.save':
        return _saveProfile(args);
      case 'profile.avatar':
        return _publishAvatar(args['remove'] == true);
      case 'profile.mood':
        if (args['clear'] == true) {
          await _rpc('retract_mood');
        } else {
          await _rpc('publish_mood', {
            'kind': _required(args, 'kind'),
            'text': _optional(args['text']),
          });
        }
        _profile['mood'] = args['clear'] == true
            ? null
            : {'kind': args['kind'], 'text': _optional(args['text'])};
        _emitProfile();
        return null;
      case 'profile.activity':
        if (args['clear'] == true) {
          await _rpc('retract_activity');
        } else {
          await _rpc('publish_activity', {
            'general': _required(args, 'general'),
            'specific': _optional(args['specific'])
                ?.toLowerCase()
                .replaceAll(RegExp(r'[^a-z0-9]+'), '_')
                .replaceAll(RegExp(r'^_+|_+$'), ''),
            'text': _optional(args['text']),
          });
        }
        _profile['activity'] = args['clear'] == true
            ? null
            : {
                'general': args['general'],
                'specific': args['specific'],
                'text': _optional(args['text']),
              };
        _emitProfile();
        return null;
      case 'profile.tune':
        return _publishTune(args);
      case 'room.create':
        final name = _required(args, 'name');
        final localpart = name
            .toLowerCase()
            .replaceAll(RegExp(r'[^a-z0-9]+'), '-')
            .replaceAll(RegExp(r'^-+|-+$'), '');
        if (localpart.isEmpty) {
          throw ArgumentError('Choose a room name with letters or numbers.');
        }
        final jid = await _rpc('create_room', {
          'localpart': localpart,
          'nick': _nick,
          'patch': {
            'name': name,
            'description': _optional(args['description']),
            'forum': args['forum'] == true,
            'pin_permission': null,
          },
        });
        if (jid is! String || !jid.contains('@')) {
          throw StateError('The server did not return a room address.');
        }
        await _refreshHome();
        return {'jid': jid};
      case 'room.config':
        final config = runtimeMap(
          await _rpc('fetch_room_config', {'room_jid': _required(args, 'jid')}),
        );
        if (config.isEmpty) {
          throw StateError('The room settings could not be loaded.');
        }
        return {
          'name': config['name'],
          'description': config['description'],
          'forum': config['forum'],
          'membersOnly': config['members_only'],
          'publicRoom': config['public_room'],
          'moderated': config['moderated'],
          'pinPermission': config['pin_permission'],
        };
      case 'room.save':
        await _rpc('submit_room_config', {
          'room_jid': _required(args, 'jid'),
          'patch': {
            'name': args['name'],
            'description': args['description'],
            'forum': args['forum'],
            'pin_permission': args['pinPermission'],
          },
        });
        await _refreshHome();
        return null;
      case 'room.destroy':
        await _rpc('destroy_room', {
          'room_jid': _required(args, 'jid'),
          'reason': _optional(args['reason']),
        });
        await _refreshHome();
        return null;
      case 'room.affiliation':
        await _rpc('set_room_affiliation', {
          'room_jid': _required(args, 'jid'),
          'target_jid': _required(args, 'targetJid'),
          'affiliation': _required(args, 'affiliation'),
          'reason': _optional(args['reason']),
        });
        return null;
      case 'room.kick':
        await _rpc('kick_occupant', {
          'room_jid': _required(args, 'jid'),
          'nick': _required(args, 'nick'),
          'reason': _optional(args['reason']),
        });
        return null;
      case 'group.create':
        final input = args['members'];
        if (input is! List ||
            input.any((value) => value is! String || !value.contains('@'))) {
          throw ArgumentError('Choose group members.');
        }
        final members = <String>{_self, ...input.cast<String>().map(bareJid)};
        if (members.length < 2) {
          throw ArgumentError('Choose at least one other person.');
        }
        final room = await _rpc('create_group_dm', {
          'name': args['name'] ?? '',
          'member_jids': members.toList(),
        });
        if (room is! String || !room.contains('@')) {
          throw StateError('The server did not return a group address.');
        }
        await _rpc('join_room', {'room_jid': room, 'nick': _nick});
        await _refreshHome();
        return {'jid': room};
      case 'group.rename':
        await _rpc('rename_group_dm', {
          'room_jid': _required(args, 'jid'),
          'name': _optional(args['name']),
        });
        await _refreshHome();
        return null;
      case 'group.invite':
        await _rpc('invite_to_group_dm', {
          'room_jid': _required(args, 'jid'),
          'invitee_jid': _required(args, 'inviteeJid'),
          'history': args['fullHistory'] == true ? 'full' : 'from_join',
        });
        return null;
      case 'group.leave':
        await _rpc('leave_group_dm', {'room_jid': _required(args, 'jid')});
        await _refreshHome();
        return null;
      case 'stickers.load':
        return _loadStickers();
      case 'stickers.remove':
        await _rpc('retract_sticker_pack', {
          'pack_id': _required(args, 'packId'),
        });
        await _loadStickers();
        return null;
      case 'stickers.create':
        return _createStickers(args);
      case 'slash.list':
        _commands ??= runtimeRows(await _rpc('discover_extension_commands'));
        return _commands!
            .map(
              (command) => {
                'serviceJid': command['service_jid'],
                'node': command['node'],
                'name': command['name'],
                'scope': command['scope'],
                'composerPrefix': command['composer_prefix'],
                'inlineField': command['inline_field'],
                'composerExecute': command['composer_execute'],
              },
            )
            .toList();
      case 'slash.invoke':
        final service = _required(args, 'serviceJid'),
            node = _required(args, 'node');
        return _commandResult(
          await _rpc('invoke_extension_command', {
            'service_jid': service,
            'node': node,
            'room_jid': _optional(args['jid']),
          }),
          service,
          node,
        );
      case 'slash.submit':
        return _submitCommand(args);
      case 'gif.search':
        return _gifSearch('${args['query'] ?? ''}');
      default:
        throw UnsupportedError('Unsupported application command: $method');
    }
  }

  Future<Map<String, dynamic>> _loadProfile(String? requested) async {
    final owner = bareJid(requested ?? _self);
    if (owner != _self) {
      final card = runtimeMap(await _rpc('fetch_vcard4', {'jid': owner}));
      final avatar = await _avatar(owner);
      return {
        'jid': owner,
        'fullName': card['full_name'] ?? owner.split('@').first,
        'nickname': card['nickname'],
        'pronouns': card['pronouns'],
        'note': card['note'],
        'url': card['url'],
        'avatar': avatar,
      };
    }
    final card = runtimeMap(await _rpc('fetch_vcard4', {'jid': owner}));
    final value = <String, dynamic>{
      'jid': owner,
      'fullName': card['full_name'],
      'nickname': card['nickname'],
      'pronouns': card['pronouns'],
      'note': card['note'],
      'url': card['url'],
      'photoUri': card['photo_uri'],
    };
    try {
      final status = runtimeMap(
        await _rpc('fetch_user_pep_profile', {'jid': owner}),
      );
      final tune = runtimeMap(status['tune']);
      value.addAll({
        'mood': status['mood'],
        'activity': status['activity'],
        'tune': tune.isEmpty
            ? null
            : {...tune, 'lengthSeconds': tune['length_seconds']},
      });
    } on AccountChanged {
      rethrow;
    } catch (_) {
      /* Absent PEP status is not a profile failure. */
    }
    try {
      value['avatar'] = await _avatar(owner);
    } on AccountChanged {
      rethrow;
    } catch (_) {
      value['avatar'] = _profile['avatar'];
    }
    _profile = value;
    _profileLoaded = true;
    _emitProfile();
    return Map.of(value);
  }

  Future<Uint8List?> _avatar(String owner) async {
    final known = _avatars[owner] ?? <String, Uint8List>{};
    final result = await _rpc('request_avatar', {
      'jid': owner,
      'known_ids': known.keys.toList(),
    });
    if (result == null) {
      _avatarCurrent.remove(owner);
      return null;
    }
    final response = runtimeMap(result);
    final id = response['id'];
    if (id is! String) {
      throw StateError('The avatar response was invalid.');
    }
    final avatar = runtimeMap(response['avatar']);
    final bytes = avatar['data'];
    if (bytes is List) {
      final encoded = Uint8List.fromList(bytes.cast<int>());
      if (encoded.length > 4 * 1024 * 1024) {
        throw StateError('This avatar is too large.');
      }
      known[id] = encoded;
      while (known.length > 4) {
        known.remove(known.keys.first);
      }
      _avatars[owner] = known;
    }
    _avatarCurrent[owner] = id;
    return known[id];
  }

  Future<void> _saveProfile(Map<String, dynamic> args) async {
    if (!_profileLoaded) {
      throw StateError('Load your profile before saving.');
    }
    final captured = context.generation;
    final previous = Map<String, dynamic>.of(_profile);
    final fields = <String, dynamic>{};
    for (final (key, limit) in [
      ('fullName', 160),
      ('nickname', 80),
      ('pronouns', 48),
      ('note', 500),
      ('url', 240),
    ]) {
      final value = _optional(args[key]);
      fields[key] = value?.substring(
        0,
        value.length > limit ? limit : value.length,
      );
    }
    _profile.addAll(fields);
    _emitProfile();
    try {
      await _rpc('publish_vcard4', {
        'vcard': {
          'full_name': fields['fullName'],
          'nickname': fields['nickname'],
          'pronouns': fields['pronouns'],
          'note': fields['note'],
          'url': fields['url'],
          'photo_uri': previous['photoUri'],
        },
      });
    } catch (_) {
      if (captured == context.generation) {
        _profile = previous;
        _emitProfile();
      }
      rethrow;
    }
  }

  Future<void> _publishAvatar(bool remove) async {
    if (remove) {
      await _rpc('disable_avatar');
      _avatarCurrent.remove(_self);
      _profile['avatar'] = null;
      _emitProfile();
      return;
    }
    final value = await _platform('profile.pickAvatar');
    if (value == null) {
      return;
    }
    final image = runtimeMap(value);
    if (image['data'] is! List) {
      throw StateError('The selected image could not be read.');
    }
    await _rpc('publish_avatar', {
      'data': image['data'],
      'mime_type': image['media_type'] ?? image['mimeType'] ?? 'image/png',
      'width': image['width'],
      'height': image['height'],
    });
    _profile['avatar'] = Uint8List.fromList(
      (image['data'] as List).cast<int>(),
    );
    _emitProfile();
  }

  Future<void> _publishTune(Map<String, dynamic> args) async {
    if (args['clear'] == true) {
      await _rpc('retract_tune');
      _profile['tune'] = null;
      _emitProfile();
      return;
    }
    int? integer(dynamic value, int max) {
      if (value == null || value == '') {
        return null;
      }
      final parsed = value is num ? value.toInt() : int.tryParse('$value');
      if (parsed == null || parsed < 1 || parsed > max) {
        throw ArgumentError('Enter valid tune details.');
      }
      return parsed;
    }

    final tune = {
      'artist': _optional(args['artist']),
      'title': _optional(args['title']),
      'source': _optional(args['source']),
      'track': _optional(args['track']),
      'uri': _optional(args['uri']),
      'length_seconds': integer(
        args['lengthSeconds'] ?? args['length'],
        2147483647,
      ),
      'rating': integer(args['rating'], 10),
    };
    if (tune.values.every((value) => value == null)) {
      throw ArgumentError('Add tune details first.');
    }
    final uri = tune['uri'];
    if (uri != null && Uri.tryParse('$uri')?.hasScheme != true) {
      throw ArgumentError('Enter a complete tune link.');
    }
    await _rpc('publish_tune', {'tune': tune});
    _profile['tune'] = {...tune, 'lengthSeconds': tune['length_seconds']};
    _emitProfile();
  }

  Future<List<Map<String, dynamic>>> _loadStickers() async {
    final response = await _rpc('fetch_sticker_packs', {'owner_jid': null});
    if (response is! List) {
      throw StateError('Sticker packs could not be loaded.');
    }
    _packs = runtimeRows(response)
        .map(
          (pack) => <String, dynamic>{
            ...pack,
            'stickers': runtimeRows(pack['stickers']).indexed
                .map(
                  (entry) => {
                    'index': entry.$1,
                    ...entry.$2,
                    'url': (entry.$2['sources'] as List?)?.firstOrNull,
                  },
                )
                .toList(),
          },
        )
        .toList();
    context.emit('stickers', {'packs': _packs});
    return _packs;
  }

  Future<dynamic> _createStickers(Map<String, dynamic> args) async {
    final name = _required(args, 'name');
    final prepared = await _platform('stickers.prepare', args);
    if (prepared == null) {
      return null;
    }
    final pack = runtimeMap(prepared);
    if (pack['stickers'] is! List || (pack['stickers'] as List).isEmpty) {
      throw StateError('Choose at least one sticker.');
    }
    final id = await _rpc('publish_sticker_pack', {
      'pack': {
        ...pack,
        'id': '',
        'name': name,
        'summary': _optional(args['summary']),
      },
    });
    await _loadStickers();
    return {'id': id};
  }

  Future<List<Map<String, dynamic>>> _gifSearch(String query) async {
    final captured = context.generation;
    final origin = Uri.tryParse('${context.session['serverOrigin'] ?? ''}');
    if (origin == null ||
        !['https', 'http'].contains(origin.scheme) ||
        origin.host.isEmpty) {
      throw StateError('GIF search is not configured for this account.');
    }
    final response = await _http
        .get(
          origin
              .resolve('/api/giphy')
              .replace(
                queryParameters: {
                  'limit': '24',
                  if (query.trim().isNotEmpty) 'q': query.trim(),
                },
              ),
        )
        .timeout(const Duration(seconds: 15));
    requireGeneration(context, captured);
    if (response.statusCode == 404 || response.statusCode == 503) {
      throw StateError('GIF search is not configured on this server.');
    }
    if (response.statusCode == 429) {
      throw StateError('GIF search is busy. Please try again shortly.');
    }
    if (response.statusCode < 200 || response.statusCode >= 300) {
      throw StateError('GIF search is unavailable.');
    }
    final result = runtimeMap(jsonDecode(response.body));
    if (result['data'] is! List) {
      throw StateError('The GIF search response was invalid.');
    }
    return runtimeRows(result['data'])
        .map((gif) {
          final images = runtimeMap(gif['images']);
          return <String, dynamic>{
            'id': gif['id'],
            'title': gif['title'] ?? '',
            'previewUrl': runtimeMap(images['fixed_height_small'])['url'],
            'originalUrl': runtimeMap(images['original'])['url'],
          };
        })
        .where(
          (gif) =>
              gif['id'] is String &&
              gif['previewUrl'] is String &&
              gif['originalUrl'] is String,
        )
        .toList();
  }

  String _formKey(String service, String node, dynamic session) =>
      '$service\u0000$node\u0000${session ?? ''}';
  Map<String, dynamic> _commandResult(
    dynamic value,
    String service,
    String node,
  ) {
    final response = runtimeMap(value);
    if (response['status'] == null) {
      throw StateError('The extension returned an invalid response.');
    }
    final form = runtimeMap(response['form']);
    final fields = runtimeRows(
      form['fields'],
    ).where((field) => field['blocked'] != true).toList();
    _forms[_formKey(service, node, response['session_id'])] = fields
        .map((field) => '${field['name']}')
        .toSet();
    return {
      ...response,
      'sessionId': response['session_id'],
      'form': form.isEmpty
          ? null
          : {
              ...form,
              'fields': fields
                  .map(
                    (field) => {
                      ...field,
                      'type': field['field_type'] ?? field['type'],
                    },
                  )
                  .toList(),
            },
    };
  }

  Future<Map<String, dynamic>> _submitCommand(Map<String, dynamic> args) async {
    final service = _required(args, 'serviceJid'),
        node = _required(args, 'node');
    final allowed = _forms[_formKey(service, node, args['sessionId'])];
    if (allowed == null) {
      throw StateError('Open the extension form before submitting it.');
    }
    final fields = runtimeRows(args['fields']);
    if (fields.any((field) => !allowed.contains(field['name']))) {
      throw ArgumentError('The extension form changed. Open it again.');
    }
    return _commandResult(
      await _rpc('submit_extension_command_form', {
        'service_jid': service,
        'node': node,
        'session_id': args['sessionId'],
        'fields': fields,
        'action': args['action'] ?? 'complete',
        'room_jid': _optional(args['jid']),
      }),
      service,
      node,
    );
  }

  Future<void> onEvent(Map<String, dynamic> event) async {
    final data = runtimeMap(event['data']);
    if (event['type'] == 'avatarChanged') {
      final owner = data['jid'];
      if (owner is! String) {
        return;
      }
      final normalized = bareJid(owner);
      if (data['avatar_id'] == null) {
        _avatarCurrent.remove(normalized);
        if (normalized == context.jid) {
          _profile['avatar'] = null;
          _emitProfile();
        }
      } else {
        try {
          final bytes = await _avatar(normalized);
          if (normalized == context.jid) {
            _profile['avatar'] = bytes;
            _emitProfile();
          }
        } on AccountChanged {
          rethrow;
        } catch (_) {
          /* Keep the last readable avatar on a transient failure. */
        }
      }
    }
  }

  void onAccountChanged() {
    _profile = {};
    _profileLoaded = false;
    _avatars.clear();
    _avatarCurrent.clear();
    _forms.clear();
    _commands = null;
    _packs = [];
  }

  void dispose() {
    _disposed = true;
    _http.close();
    onAccountChanged();
  }
}

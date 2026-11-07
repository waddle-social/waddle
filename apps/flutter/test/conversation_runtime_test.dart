import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_flutter/runtime/context.dart';
import 'package:waddle_flutter/runtime/conversation_runtime.dart';
import 'package:waddle_flutter/transport/transport.dart';
import 'package:waddle_flutter/transport/transport_native.dart';
import 'package:waddle_flutter/transport/native_bindings.dart';

const room = 'general@rooms.example.org';
const own = 'me@example.org';
const viewArgs = {
  'jid': room,
  'group': true,
  'viewId': 'room-view',
  'atNewestEdge': false,
};

class _Transport implements RustTransport {
  @override
  Stream<Map<String, dynamic>> get events => const Stream.empty();
  @override
  Future<dynamic> rpc(
    String method, [
    Map<String, dynamic> args = const {},
  ]) async => throw UnimplementedError();
  @override
  Future<void> close() async {}
}

class _Context implements RuntimeContext {
  _Context({this.realTimeline});
  final RustTransport? realTimeline;
  final calls = <(String, Map<String, dynamic>)>[];
  final emissions = <(String, Map<String, dynamic>)>[];
  final timelines = <String, List<Map<String, dynamic>>>{};
  final responses = <String, dynamic>{};
  final callbacks = <String, Future<dynamic> Function(Map<String, dynamic>)>{};
  bool persistenceFails = false;
  int persists = 0;
  @override
  RustTransport? transport = _Transport();
  @override
  String? jid = own;
  @override
  int generation = 1;
  @override
  Map<String, dynamic> preferences = {
    'readReceiptsEnabled': true,
    'timeline': 'chat',
  };
  @override
  Map<String, dynamic> session = {'jid': own, 'xmppLocalpart': 'me'};
  @override
  Map<String, dynamic> persistent = {};
  @override
  void emit(String topic, Map<String, dynamic> data) =>
      emissions.add((topic, data));
  @override
  Future<dynamic> platform(
    String method, [
    Map<String, dynamic> args = const {},
  ]) async {
    calls.add((method, args));
    if (callbacks.containsKey(method)) {
      return callbacks[method]!(args);
    }
    return responses[method];
  }

  @override
  Future<void> persist() async {
    persists++;
    if (persistenceFails) {
      throw StateError('Storage failed');
    }
    persistent = jsonDecode(jsonEncode(persistent)) as Map<String, dynamic>;
  }

  @override
  Future<void> refreshHome() async {}
  @override
  Future<dynamic> rpc(
    String method, [
    Map<String, dynamic> args = const {},
  ]) async {
    calls.add((method, Map.from(args)));
    if (callbacks.containsKey(method)) {
      return callbacks[method]!(args);
    }
    if (responses.containsKey(method)) {
      return responses[method];
    }
    if (method.startsWith('timeline_') && realTimeline != null) {
      return realTimeline!.rpc(method, args);
    }
    switch (method) {
      case 'timeline_reset':
        timelines.clear();
        return {'items': []};
      case 'timeline_snapshot':
        return {'items': timelines[args['conversation_jid']] ?? []};
      case 'timeline_ingest':
        final items = timelines.putIfAbsent(
          args['conversation_jid'] as String,
          () => [],
        );
        for (final message in runtimeRows(args['messages'])) {
          // This double represents the Rust canonical boundary. Its authority
          // rules are separately exercised against the actual native library.
          final origin = message['origin_id'];
          final incoming = _canonical(message);
          final index = items.indexWhere(
            (row) =>
                origin != null && row['origin_id'] == origin ||
                row['id'] == incoming['id'],
          );
          if (index < 0) {
            items.add(incoming);
          } else {
            items[index] = incoming;
          }
        }
        items.sort(
          (a, b) =>
              (a['timestamp_ms'] as int).compareTo(b['timestamp_ms'] as int),
        );
        return {'items': items};
      case 'timeline_local_echo':
        final items = timelines.putIfAbsent(
          args['conversation_jid'] as String,
          () => [],
        );
        if (!items.any((row) => row['origin_id'] == args['origin_id'])) {
          items.add({
            'id': args['origin_id'],
            'origin_id': args['origin_id'],
            'message_id': args['origin_id'],
            'reference_id': args['origin_id'],
            'correction_id': args['origin_id'],
            'sender': 'me',
            'own': true,
            'body': args['body'],
            'timestamp_ms': args['timestamp_ms'],
            'thread': args['thread'],
            'delivery': 'sending',
            'reactions': [],
            'attachments': [],
          });
        }
        return {'items': items};
      case 'timeline_delivery':
        final items = timelines[args['conversation_jid']] ?? [];
        for (final row in items) {
          if (row['origin_id'] == args['origin_id']) {
            row['delivery'] = args['delivery'];
          }
        }
        return {'items': items};
      case 'timeline_own_reactions':
        final items = timelines[args['conversation_jid']] ?? [];
        return {'items': items};
      case 'fetch_room_history':
      case 'fetch_room_thread_history':
      case 'fetch_dm_history':
        return {
          'messages': [],
          'is_complete': true,
          'first_id': null,
          'last_id': null,
        };
      case 'fetch_room_pins':
        return [];
      case 'send_groupchat_message':
      case 'send_chat_message':
        return {
          'type': 'sent',
          'stanza_id': runtimeMap(args['options'])['stanza_id'],
        };
      case 'join_room':
      case 'mark_inbox_read':
        return null;
      default:
        return true;
    }
  }

  Map<String, dynamic> get conversation =>
      emissions.where((event) => event.$1 == 'conversation').last.$2;
  List<Map<String, dynamic>> recorded(String method) => calls
      .where((entry) => entry.$1 == method)
      .map((entry) => entry.$2)
      .toList();
}

Map<String, dynamic> _canonical(Map<String, dynamic> raw) => {
  'id': raw['stanza_id'] ?? raw['origin_id'] ?? raw['id'],
  'origin_id': raw['origin_id'],
  'message_id': raw['id'],
  'reference_id':
      raw['trusted_reference'] ??
      raw['stanza_id'] ??
      raw['origin_id'] ??
      raw['id'],
  'correction_id': raw['origin_id'] ?? raw['id'],
  'archive_id': raw['stanza_id'],
  'archive_by': raw['stanza_id_by'],
  'sender': (raw['from'] as String? ?? '').split('/').last,
  'author_id': raw['author_real_jid'],
  'body': raw['body'] ?? '',
  'own': raw['mine'] == true,
  'timestamp_ms': DateTime.parse(
    raw['timestamp'] as String,
  ).millisecondsSinceEpoch,
  'thread': raw['thread'],
  'marker_requested': raw['displayed_marker_requested'] == true,
  'retracted': raw['is_retracted'] == true,
  'reactions': [],
  'attachments': [],
};

Map<String, dynamic> _message(String id, {String? origin, bool mine = false}) =>
    {
      'id': 'author-$id',
      'origin_id': origin,
      'stanza_id': id,
      'stanza_id_by': room,
      'from': '$room/${mine ? 'me' : 'alex'}',
      'message_type': 'groupchat',
      'is_muc': true,
      'body': 'Message $id',
      'timestamp': '2026-10-31T10:00:00Z',
      'mine': mine,
    };

Future<void> _flush() async {
  for (var index = 0; index < 12; index++) {
    await Future<void>.delayed(Duration.zero);
  }
}

Future<void> _start(ConversationRuntime runtime) =>
    runtime.onEvent({'type': 'connected', 'data': {}});

Future<void> _selfPresence(ConversationRuntime runtime) => runtime.onEvent({
  'type': 'presence',
  'data': {
    'from': '$room/me',
    'muc_jid': own,
    'muc_status_codes': [110],
  },
});

void main() {
  test(
    'queued send is durable before wire and survives reconnect with the same origin',
    () async {
      final context = _Context();
      final first = ConversationRuntime(context);
      await first.invoke('conversation.open', viewArgs);
      await first.invoke('conversation.send', {
        ...viewArgs,
        'body': 'Offline message',
      });
      expect(context.recorded('send_groupchat_message'), isEmpty);
      final queue = runtimeRows(context.persistent['conversationOutbox']);
      expect(queue, hasLength(1));
      final id = queue.single['origin_id'];
      first.dispose();
      final restored = ConversationRuntime(context);
      addTearDown(restored.dispose);
      await _start(restored);
      expect(
        runtimeMap(
          context.recorded('send_groupchat_message').single['options'],
        )['stanza_id'],
        id,
      );
      expect(context.recorded('join_room'), hasLength(1));
    },
  );

  test(
    'room typing ignores our own reflected chat states but shows occupants',
    () async {
      final context = _Context();
      final runtime = ConversationRuntime(context);
      addTearDown(runtime.dispose);
      await _start(runtime);
      await runtime.invoke('conversation.open', viewArgs);
      for (final presence in [
        {
          'from': '$room/Me Renamed',
          'muc_jid': own,
          'muc_status_codes': [110],
        },
        {'from': '$room/me-laptop', 'muc_jid': '$own/laptop'},
        {'from': '$room/alex', 'muc_jid': 'alex@example.org'},
      ]) {
        await runtime.onEvent({'type': 'presence', 'data': presence});
      }
      Future<void> composing(String nick) => runtime.onEvent({
        'type': 'message',
        'data': {
          'from': '$room/$nick',
          'message_type': 'groupchat',
          'is_muc': true,
          'chat_state': 'composing',
        },
      });
      // XEP-0045 reflects our own state from our (room-assigned) nick; our
      // other device is identified by its disclosed real JID.
      await composing('Me Renamed');
      await composing('me-laptop');
      expect(context.conversation['typing'], isEmpty);
      await composing('alex');
      expect(context.conversation['typing'], ['alex']);
    },
  );

  test(
    'a replacement transport replays while the old ready handler still awaits a send',
    () async {
      final context = _Context();
      final oldSend = Completer<dynamic>();
      var sends = 0;
      context.callbacks['send_groupchat_message'] = (args) async {
        if (++sends == 1) {
          return oldSend.future;
        }
        return {
          'type': 'sent',
          'stanza_id': runtimeMap(args['options'])['stanza_id'],
        };
      };
      final runtime = ConversationRuntime(context);
      addTearDown(runtime.dispose);
      await runtime.invoke('conversation.open', viewArgs);
      await runtime.invoke('conversation.send', {
        ...viewArgs,
        'body': 'Survive stream replacement',
      });
      final firstReady = _start(runtime);
      await _until(() => sends == 1);
      context.transport = _Transport();
      await _start(runtime).timeout(const Duration(seconds: 2));
      expect(sends, 2);
      final payloads = context.recorded('send_groupchat_message');
      final origin = runtimeMap(payloads.first['options'])['stanza_id'];
      expect(runtimeMap(payloads.last['options'])['stanza_id'], origin);
      oldSend.complete({'type': 'invalid_options'});
      await firstReady;
      await _flush();
      final pending = runtimeRows(
        context.persistent['conversationOutbox'],
      ).single;
      expect(pending['failed'], isFalse);
      expect(pending['status'], 'sent');
    },
  );

  test(
    'storage failure prevents every wire send and keeps the draft retryable',
    () async {
      final context = _Context();
      final runtime = ConversationRuntime(context);
      addTearDown(runtime.dispose);
      await _start(runtime);
      await runtime.invoke('conversation.open', viewArgs);
      await _flush();
      context.persistenceFails = true;
      await expectLater(
        runtime.invoke('conversation.send', {
          ...viewArgs,
          'body': 'Do not lose me',
        }),
        throwsStateError,
      );
      expect(context.recorded('send_groupchat_message'), isEmpty);
    },
  );

  test(
    'ack before send completion still requires a room reflection and retains ownership on navigation',
    () async {
      final context = _Context();
      final send = Completer<dynamic>();
      context.callbacks['send_groupchat_message'] = (_) => send.future;
      final runtime = ConversationRuntime(context);
      addTearDown(runtime.dispose);
      await _start(runtime);
      await _selfPresence(runtime);
      await runtime.invoke('conversation.open', viewArgs);
      await _flush();
      await runtime.invoke('conversation.send', {
        ...viewArgs,
        'body': 'Race me',
      });
      await _flush();
      final id =
          runtimeMap(
                context.recorded('send_groupchat_message').single['options'],
              )['stanza_id']
              as String;
      await runtime.onEvent({
        'type': 'deliveryAcked',
        'data': {'stanza_id': id},
      });
      expect(
        runtimeRows(context.conversation['messages']).single['pending'],
        isTrue,
      );
      expect(
        runtimeRows(context.conversation['messages']).single['acked'],
        isTrue,
      );
      await runtime.invoke('conversation.close', viewArgs);
      await runtime.onEvent({
        'type': 'message',
        'data': _message('room-stamp', origin: id, mine: true),
      });
      send.complete({'type': 'sent', 'stanza_id': id});
      await _flush();
      await runtime.invoke('conversation.open', {
        ...viewArgs,
        'viewId': 'new-view',
      });
      await _flush();
      expect(
        runtimeRows(context.conversation['messages']).single['pending'],
        isFalse,
      );
      expect(runtimeRows(context.persistent['conversationOutbox']), isEmpty);
      await runtime.onEvent({
        'type': 'messageRejected',
        'data': {'stanza_id': id, 'from': room, 'to': own},
      });
      expect(runtimeRows(context.persistent['conversationOutbox']), isEmpty);
      expect(
        runtimeRows(context.conversation['messages']).single['pending'],
        isFalse,
      );
    },
  );

  test(
    'rejections require both retained outbound ID and authorized sender address',
    () async {
      final context = _Context();
      final runtime = ConversationRuntime(context);
      addTearDown(runtime.dispose);
      await _start(runtime);
      await _selfPresence(runtime);
      await runtime.invoke('conversation.open', viewArgs);
      await _flush();
      await runtime.invoke('conversation.send', {
        ...viewArgs,
        'body': 'Owned send',
      });
      await _flush();
      final id =
          runtimeMap(
                context.recorded('send_groupchat_message').single['options'],
              )['stanza_id']
              as String;
      await runtime.onEvent({
        'type': 'messageRejected',
        'data': {'stanza_id': id, 'from': '$room/attacker', 'to': own},
      });
      expect(
        runtimeRows(context.conversation['messages']).single['failed'],
        isFalse,
      );
      await runtime.onEvent({
        'type': 'messageRejected',
        'data': {'stanza_id': id, 'from': room, 'to': own},
      });
      expect(
        runtimeRows(context.conversation['messages']).single['failed'],
        isTrue,
      );
      expect(
        runtimeRows(context.conversation['messages']).single['rejected'],
        isTrue,
      );
      // Actual acceptance proof supersedes an earlier raced rejection, while
      // ACK alone never upgrades that rejection to acceptance.
      await runtime.onEvent({
        'type': 'deliveryAcked',
        'data': {'stanza_id': id},
      });
      expect(
        runtimeRows(context.persistent['conversationOutbox']).single['failed'],
        isTrue,
      );
      await runtime.onEvent({
        'type': 'message',
        'data': _message('accepted-after-reject', origin: id, mine: true),
      });
      expect(runtimeRows(context.persistent['conversationOutbox']), isEmpty);
      expect(
        runtimeRows(context.conversation['messages']).single['pending'],
        isFalse,
      );
    },
  );

  test(
    'history is single-flight and late old-account completion cannot publish',
    () async {
      final context = _Context();
      final history = Completer<dynamic>();
      context.callbacks['fetch_room_history'] = (_) => history.future;
      final runtime = ConversationRuntime(context);
      addTearDown(runtime.dispose);
      await _start(runtime);
      await runtime.invoke('conversation.open', viewArgs);
      await _flush();
      await runtime.invoke('conversation.history', viewArgs);
      expect(context.recorded('fetch_room_history'), hasLength(1));
      context.generation++;
      context.jid = 'other@example.org';
      context.persistent = {};
      runtime.onAccountChanged();
      context.emissions.clear();
      history.complete({
        'messages': [_message('private-old')],
        'is_complete': true,
      });
      await _flush();
      expect(context.emissions, isEmpty);
      expect(context.persistent, isEmpty);
    },
  );

  test(
    'replacement history owns its cursor and loading state despite late old results',
    () async {
      final context = _Context();
      final oldHistory = Completer<dynamic>();
      var histories = 0;
      context.callbacks['fetch_room_history'] = (_) async {
        if (++histories == 1) {
          return oldHistory.future;
        }
        return {
          'messages': [_message('fresh-stream')],
          'is_complete': true,
        };
      };
      final runtime = ConversationRuntime(context);
      addTearDown(runtime.dispose);
      await _start(runtime);
      await runtime.invoke('conversation.open', viewArgs);
      await _until(() => histories == 1);
      context.transport = _Transport();
      await _start(runtime);
      oldHistory.complete({
        'messages': [_message('stale-stream')],
        'is_complete': false,
      });
      await _flush();
      expect(histories, 2);
      expect(
        runtimeRows(context.conversation['messages']).single['id'],
        'fresh-stream',
      );
      expect(context.conversation['loadingOlder'], isFalse);
      expect(context.conversation['reachedStart'], isTrue);
      expect(context.emissions.where((entry) => entry.$1 == 'error'), isEmpty);
    },
  );

  test(
    'read markers use canonical trusted identity only at the visible live edge',
    () async {
      final context = _Context();
      context.responses['fetch_room_history'] = {
        'messages': [
          {..._message('archive-id'), 'trusted_reference': 'trusted-room-id'},
        ],
        'is_complete': true,
      };
      final runtime = ConversationRuntime(context);
      addTearDown(runtime.dispose);
      await _start(runtime);
      await runtime.invoke('conversation.open', viewArgs);
      await _flush();
      expect(context.recorded('send_displayed'), isEmpty);
      await runtime.invoke('conversation.visibility', {
        ...viewArgs,
        'visible': false,
      });
      await runtime.invoke('conversation.read', {
        ...viewArgs,
        'atNewestEdge': true,
      });
      await _flush();
      expect(context.recorded('send_displayed'), isEmpty);
      await runtime.invoke('conversation.visibility', {
        ...viewArgs,
        'visible': true,
      });
      await _flush();
      expect(
        context.recorded('send_displayed').single['stanza_id'],
        'trusted-room-id',
      );
      expect(
        context.recorded('publish_mds_displayed').single['stanza_id'],
        'archive-id',
      );
      final before = context.recorded('send_displayed').length;
      await runtime.invoke('conversation.open', {
        ...viewArgs,
        'viewId': 'thread',
        'threadId': 'trusted-room-id',
        'atNewestEdge': true,
      });
      await _flush();
      expect(context.recorded('send_displayed'), hasLength(before));
    },
  );

  test(
    'read tracking advances after an empty first read and successive messages',
    () async {
      final context = _Context();
      final runtime = ConversationRuntime(context);
      addTearDown(runtime.dispose);
      await _start(runtime);
      await runtime.invoke('conversation.open', {
        ...viewArgs,
        'atNewestEdge': true,
      });
      await _until(() => context.recorded('fetch_room_pins').isNotEmpty);
      await _flush();
      expect(context.recorded('send_displayed'), isEmpty);

      await runtime.onEvent({'type': 'message', 'data': _message('first')});
      await _until(() => context.recorded('send_displayed').length == 1);
      await _until(
        () =>
            runtimeMap(
              runtimeMap(context.persistent['conversationReadCursors'])[room],
            )['id'] ==
            'first',
      );
      await runtime.onEvent({
        'type': 'message',
        'data': {..._message('second'), 'timestamp': '2026-10-31T10:01:00Z'},
      });
      await _until(() => context.recorded('send_displayed').length == 2);
      await _until(
        () =>
            runtimeMap(
              runtimeMap(context.persistent['conversationReadCursors'])[room],
            )['id'] ==
            'second',
      );
      expect(
        context.recorded('send_displayed').map((row) => row['stanza_id']),
        ['first', 'second'],
      );
    },
  );

  test(
    'read tracking revisits a message received during an in-flight read',
    () async {
      final context = _Context();
      final runtime = ConversationRuntime(context);
      addTearDown(runtime.dispose);
      final firstRead = Completer<dynamic>();
      context.callbacks['send_displayed'] = (args) async {
        if (args['stanza_id'] == 'first') return firstRead.future;
        return true;
      };
      await _start(runtime);
      await runtime.invoke('conversation.open', {
        ...viewArgs,
        'atNewestEdge': true,
      });
      await _until(() => context.recorded('fetch_room_pins').isNotEmpty);
      await _flush();
      await runtime.onEvent({'type': 'message', 'data': _message('first')});
      await _until(() => context.recorded('send_displayed').length == 1);
      await runtime.onEvent({
        'type': 'message',
        'data': {..._message('second'), 'timestamp': '2026-10-31T10:01:00Z'},
      });
      expect(context.recorded('send_displayed'), hasLength(1));
      firstRead.complete(true);
      await _until(() => context.recorded('send_displayed').length == 2);
      expect(context.recorded('send_displayed').last['stanza_id'], 'second');
    },
  );

  test('a failed read does not block a later explicit retry', () async {
    final context = _Context();
    final runtime = ConversationRuntime(context);
    addTearDown(runtime.dispose);
    var attempts = 0;
    context.callbacks['send_displayed'] = (_) async {
      if (++attempts == 1) throw StateError('temporary failure');
      return true;
    };
    await _start(runtime);
    await runtime.invoke('conversation.open', {
      ...viewArgs,
      'atNewestEdge': true,
    });
    await _until(() => context.recorded('fetch_room_pins').isNotEmpty);
    await _flush();
    await runtime.onEvent({'type': 'message', 'data': _message('retry')});
    await _until(() => context.emissions.any((event) => event.$1 == 'error'));
    await runtime.invoke('conversation.read', {
      ...viewArgs,
      'atNewestEdge': true,
    });
    await _until(() => attempts == 2);
    await _until(
      () =>
          runtimeMap(
            runtimeMap(context.persistent['conversationReadCursors'])[room],
          )['id'] ==
          'retry',
    );
  });

  test('inserted content passes only supplied file metadata', () async {
    final context = _Context();
    final runtime = ConversationRuntime(context);
    addTearDown(runtime.dispose);
    await runtime.invoke('conversation.open', viewArgs);
    await runtime.invoke('conversation.insertContent', {
      ...viewArgs,
      'data': Uint8List.fromList([1, 2, 3]),
    });
    final bytes = context.recorded('file.uploadBytes').single;
    expect(bytes, isNot(contains('name')));
    expect(bytes, isNot(contains('media_type')));
    await runtime.invoke('conversation.insertContent', {
      ...viewArgs,
      'uri': 'content://keyboard/image',
      'name': 'photo.webp',
      'mimeType': 'image/webp',
    });
    final uri = context.recorded('file.uploadUri').single;
    expect(uri['name'], 'photo.webp');
    expect(uri['media_type'], 'image/webp');
  });

  test(
    'real timeline retains metadata for resource-qualified direct messages',
    () async {
      final transport = NativeRustTransport(FfiRustBindings.load());
      addTearDown(transport.close);
      final context = _Context(realTimeline: transport)..transport = transport;
      final runtime = ConversationRuntime(context);
      addTearDown(runtime.dispose);
      const args = {
        'jid': 'alice@example.org',
        'group': false,
        'viewId': 'dm',
        'atNewestEdge': false,
      };
      await _start(runtime);
      await runtime.invoke('conversation.open', args);
      await _until(() => context.recorded('fetch_dm_history').isNotEmpty);
      await _flush();
      await runtime.onEvent({
        'type': 'message',
        'data': {
          'id': 'direct',
          'origin_id': 'direct',
          'from': 'alice@example.org/phone',
          'to': '$own/desktop',
          'message_type': 'chat',
          'body': 'Bold words',
          'timestamp': '2026-10-31T10:00:00Z',
          'markup_spans': [
            {'start': 0, 'end': 4, 'span_type': 'bold'},
          ],
          'link_previews': [
            {'original_url': 'https://example.org/', 'title': 'preview'},
          ],
          'shared_files': [
            {
              'url': 'https://files.example.org/file.txt',
              'name': 'file.txt',
              'media_type': 'text/plain',
              'size': 3,
              'hashes': [
                {
                  'algo': 'sha-256',
                  'value_b64': 'CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=',
                },
              ],
              'encrypted': null,
            },
          ],
        },
      });
      final row = runtimeRows(context.conversation['messages']).single;
      expect(runtimeRows(row['markup']), hasLength(1));
      expect(runtimeRows(row['linkPreviews']).single['title'], 'preview');
      await runtime.invoke('conversation.fileOpen', {
        ...args,
        'targetId': 'direct',
        'fileId': '0',
      });
      expect(
        runtimeRows(
          runtimeMap(context.recorded('file.open').single['file'])['hashes'],
        ),
        hasLength(1),
      );
      await runtime.onEvent({
        'type': 'message',
        'data': {
          'id': 'other',
          'origin_id': 'other',
          'stanza_id': 'direct',
          'stanza_id_by': 'attacker.example.org',
          'from': 'alice@example.org/phone',
          'to': '$own/desktop',
          'message_type': 'chat',
          'body': 'Other',
          'timestamp': '2026-10-31T10:01:00Z',
          'link_previews': [
            {'original_url': 'https://attacker.example.org/', 'title': 'spoof'},
          ],
        },
      });
      final original = runtimeRows(
        context.conversation['messages'],
      ).singleWhere((row) => row['id'] == 'direct');
      expect(runtimeRows(original['linkPreviews']).single['title'], 'preview');
    },
    skip: Platform.environment['WADDLE_FLUTTER_RUNTIME_LIBRARY'] == null
        ? 'Build the real Rust runtime and set WADDLE_FLUTTER_RUNTIME_LIBRARY.'
        : false,
  );

  test(
    'room projection does not attach a new nickname owners colliding metadata',
    () async {
      final transport = NativeRustTransport(FfiRustBindings.load());
      addTearDown(transport.close);
      final context = _Context(realTimeline: transport)..transport = transport;
      final runtime = ConversationRuntime(context);
      addTearDown(runtime.dispose);
      await _start(runtime);
      await runtime.invoke('conversation.open', viewArgs);
      await _until(() => context.recorded('fetch_room_pins').isNotEmpty);
      await runtime.onEvent({
        'type': 'presence',
        'data': {'from': '$room/alex', 'muc_jid': 'alice@example.org'},
      });
      await runtime.onEvent({
        'type': 'message',
        'data': {
          ..._message('original', origin: 'chosen-id'),
          'link_previews': [
            {
              'original_url': 'https://example.org/original',
              'title': 'original',
            },
          ],
        },
      });
      await runtime.onEvent({
        'type': 'presence',
        'data': {'from': '$room/alex', 'muc_jid': 'mallory@example.org'},
      });
      await runtime.onEvent({
        'type': 'message',
        'data': {
          ..._message('attacker', origin: 'chosen-id'),
          'link_previews': [
            {
              'original_url': 'https://example.org/attacker',
              'title': 'attacker',
            },
          ],
        },
      });
      final original = runtimeRows(
        context.conversation['messages'],
      ).singleWhere((row) => row['id'] == 'original');
      expect(runtimeRows(original['linkPreviews']).single['title'], 'original');
      await runtime.onEvent({
        'type': 'presence',
        'data': {'from': '$room/alex', 'muc_jid': 'alice@example.org'},
      });
      await runtime.onEvent({
        'type': 'message',
        'data': {
          ..._message('correction', origin: 'chosen-id'),
          'replaces_id': 'chosen-id',
          'link_previews': [
            {
              'original_url': 'https://example.org/correction',
              'title': 'mutation',
            },
          ],
        },
      });
      final corrected = runtimeRows(
        context.conversation['messages'],
      ).singleWhere((row) => row['id'] == 'original');
      expect(
        runtimeRows(corrected['linkPreviews']).single['title'],
        'original',
      );
    },
    skip: Platform.environment['WADDLE_FLUTTER_RUNTIME_LIBRARY'] == null
        ? 'Build the real Rust runtime and set WADDLE_FLUTTER_RUNTIME_LIBRARY.'
        : false,
  );

  test(
    'trusted room identities keep duplicate sender origins on separate files',
    () async {
      final transport = NativeRustTransport(FfiRustBindings.load());
      addTearDown(transport.close);
      final context = _Context(realTimeline: transport)..transport = transport;
      final runtime = ConversationRuntime(context);
      addTearDown(runtime.dispose);
      await _start(runtime);
      await runtime.invoke('conversation.open', viewArgs);
      await _until(() => context.recorded('fetch_room_pins').isNotEmpty);
      Map<String, dynamic> message(String stanza, String name, String style) =>
          {
            ..._message(stanza, origin: 'shared-origin'),
            'body': 'hello',
            'markup_spans': [
              {'start': 0, 'end': 5, 'span_type': style},
            ],
            'shared_files': [
              {
                'url': 'https://files.example.org/$name',
                'name': name,
                'media_type': 'text/plain',
                'size': 3,
                'encrypted': null,
              },
            ],
          };
      await runtime.onEvent({
        'type': 'message',
        'data': message('stanza-a', 'a.txt', 'bold'),
      });
      final first = runtimeRows(context.conversation['messages']).single;
      final firstRevision = runtimeRows(
        first['files'],
      ).single['contentRevision'];
      await runtime.onEvent({
        'type': 'message',
        'data': message('stanza-b', 'b.txt', 'italic'),
      });
      final rows = runtimeRows(context.conversation['messages']);
      expect(rows, hasLength(2));
      final a = rows.singleWhere((row) => row['id'] == 'stanza-a');
      final b = rows.singleWhere((row) => row['id'] == 'stanza-b');
      expect(runtimeRows(a['markup']).single['kind'], 'bold');
      expect(runtimeRows(b['markup']).single['kind'], 'italic');
      expect(runtimeRows(a['files']).single['name'], 'a.txt');
      expect(runtimeRows(a['files']).single['contentRevision'], firstRevision);
      expect(runtimeRows(b['files']).single['name'], 'b.txt');
      expect(
        runtimeRows(b['files']).single['contentRevision'],
        isNot(firstRevision),
      );
      await expectLater(
        runtime.invoke('conversation.fileBytes', {
          ...viewArgs,
          'targetId': 'shared-origin',
          'fileId': '0',
        }),
        throwsStateError,
      );
      await runtime.invoke('conversation.fileBytes', {
        ...viewArgs,
        'targetId': 'stanza-a',
        'fileId': '0',
      });
      final file = runtimeMap(context.recorded('file.bytes').single['file']);
      expect(file['name'], 'a.txt');
      expect(file['url'], 'https://files.example.org/a.txt');
    },
    skip: Platform.environment['WADDLE_FLUTTER_RUNTIME_LIBRARY'] == null
        ? 'Build the real Rust runtime and set WADDLE_FLUTTER_RUNTIME_LIBRARY.'
        : false,
  );

  test(
    'actions reject canonical identities that serialize to the same value',
    () async {
      final transport = NativeRustTransport(FfiRustBindings.load());
      addTearDown(transport.close);
      final context = _Context(realTimeline: transport)..transport = transport;
      final runtime = ConversationRuntime(context);
      addTearDown(runtime.dispose);
      await _start(runtime);
      await runtime.invoke('conversation.open', viewArgs);
      await _until(() => context.recorded('fetch_room_pins').isNotEmpty);
      await runtime.onEvent({
        'type': 'message',
        'data': _message('collision', origin: 'origin-a'),
      });
      await runtime.onEvent({
        'type': 'message',
        'data': {
          ..._message('unused', origin: 'collision'),
          'id': 'message-b',
          'from': '$room/bob',
          'stanza_id': null,
          'stanza_id_by': null,
        },
      });
      expect(
        runtimeRows(
          context.conversation['messages'],
        ).where((row) => row['id'] == 'collision'),
        hasLength(2),
      );
      await expectLater(
        runtime.invoke('conversation.fileBytes', {
          ...viewArgs,
          'targetId': 'collision',
          'fileId': '0',
        }),
        throwsStateError,
      );
      expect(context.recorded('file.bytes'), isEmpty);
    },
    skip: Platform.environment['WADDLE_FLUTTER_RUNTIME_LIBRARY'] == null
        ? 'Build the real Rust runtime and set WADDLE_FLUTTER_RUNTIME_LIBRARY.'
        : false,
  );

  test(
    'trusted row identities outrank other messages sender-chosen aliases',
    () async {
      final transport = NativeRustTransport(FfiRustBindings.load());
      addTearDown(transport.close);
      final context = _Context(realTimeline: transport)..transport = transport;
      final runtime = ConversationRuntime(context);
      addTearDown(runtime.dispose);
      await _start(runtime);
      await runtime.invoke('conversation.open', viewArgs);
      await _until(() => context.recorded('fetch_room_pins').isNotEmpty);
      await runtime.onEvent({
        'type': 'presence',
        'data': {'from': '$room/alex', 'muc_jid': 'alice@example.org'},
      });
      Map<String, dynamic> message(
        String stanza,
        String id,
        String origin,
        String name,
        String style,
      ) => {
        ..._message(stanza, origin: origin),
        'id': id,
        'body': 'hello',
        'markup_spans': [
          {'start': 0, 'end': 5, 'span_type': style},
        ],
        'shared_files': [
          {
            'url': 'https://files.example.org/$name',
            'name': name,
            'media_type': 'text/plain',
            'size': 3,
            'encrypted': null,
          },
        ],
      };
      await runtime.onEvent({
        'type': 'message',
        'data': message('trusted-a', 'message-a', 'origin-a', 'a.txt', 'bold'),
      });
      final first = runtimeRows(context.conversation['messages']).single;
      final firstRevision = runtimeRows(
        first['files'],
      ).single['contentRevision'];
      await runtime.onEvent({
        'type': 'message',
        'data': message(
          'trusted-b',
          'trusted-a',
          'origin-b',
          'b.txt',
          'italic',
        ),
      });
      await runtime.onEvent({
        'type': 'message',
        'data': {
          ...message(
            'trusted-c',
            'message-c',
            'trusted-a',
            'c.txt',
            'strikethrough',
          ),
          'from': '$room/bob',
        },
      });
      final rows = runtimeRows(context.conversation['messages']);
      expect(rows, hasLength(3));
      final a = rows.singleWhere((row) => row['id'] == 'trusted-a');
      final b = rows.singleWhere((row) => row['id'] == 'trusted-b');
      expect(runtimeRows(a['markup']).single['kind'], 'bold');
      expect(runtimeRows(b['markup']).single['kind'], 'italic');
      expect(runtimeRows(a['files']).single['name'], 'a.txt');
      expect(runtimeRows(a['files']).single['contentRevision'], firstRevision);
      expect(runtimeRows(b['files']).single['name'], 'b.txt');
      expect(
        runtimeRows(b['files']).single['contentRevision'],
        isNot(firstRevision),
      );
      await runtime.invoke('conversation.fileBytes', {
        ...viewArgs,
        'targetId': 'trusted-a',
        'fileId': '0',
      });
      expect(
        runtimeMap(context.recorded('file.bytes').last['file'])['name'],
        'a.txt',
      );
      await runtime.invoke('conversation.fileBytes', {
        ...viewArgs,
        'targetId': 'trusted-b',
        'fileId': '0',
      });
      expect(
        runtimeMap(context.recorded('file.bytes').last['file'])['name'],
        'b.txt',
      );
    },
    skip: Platform.environment['WADDLE_FLUTTER_RUNTIME_LIBRARY'] == null
        ? 'Build the real Rust runtime and set WADDLE_FLUTTER_RUNTIME_LIBRARY.'
        : false,
  );

  test(
    'file content revisions change with the envelope without exposing secrets',
    () async {
      final transport = NativeRustTransport(FfiRustBindings.load());
      addTearDown(transport.close);
      final context = _Context(realTimeline: transport)..transport = transport;
      final runtime = ConversationRuntime(context);
      addTearDown(runtime.dispose);
      const args = {
        'jid': 'alice@example.org',
        'group': false,
        'viewId': 'dm',
        'atNewestEdge': false,
      };
      await _start(runtime);
      await runtime.invoke('conversation.open', args);
      await _until(() => context.recorded('fetch_dm_history').isNotEmpty);
      await _flush();
      Map<String, dynamic> message(String key, String source, String hash) => {
        'id': 'same',
        'origin_id': 'same',
        'from': 'alice@example.org/phone',
        'to': '$own/desktop',
        'message_type': 'chat',
        'body': 'File',
        'timestamp': '2026-10-31T10:00:00Z',
        'shared_files': [
          {
            'url': source,
            'name': 'file.png',
            'media_type': 'image/png',
            'size': 3,
            'encrypted': {
              'cipher': 'urn:xmpp:ciphers:aes-256-gcm-nopadding:0',
              'key_b64': key,
              'iv_b64': 'PRIVATE-IV',
              'hashes': [
                {'algo': 'sha-256', 'value_b64': hash},
              ],
              'sources': [source],
            },
          },
        ],
      };
      Future<dynamic> revision(String key, String source, String hash) async {
        await runtime.onEvent({
          'type': 'message',
          'data': message(key, source, hash),
        });
        return runtimeRows(
          runtimeRows(context.conversation['messages']).single['files'],
        ).single['contentRevision'];
      }

      final first = await revision(
        'PRIVATE-KEY-ONE',
        'https://files.example.org/one',
        'PRIVATE-HASH-ONE',
      );
      expect(first, isA<String>());
      expect(
        await revision(
          'PRIVATE-KEY-ONE',
          'https://files.example.org/one',
          'PRIVATE-HASH-ONE',
        ),
        first,
      );
      final newKey = await revision(
        'PRIVATE-KEY-TWO',
        'https://files.example.org/one',
        'PRIVATE-HASH-ONE',
      );
      expect(newKey, isNot(first));
      final newSource = await revision(
        'PRIVATE-KEY-TWO',
        'https://files.example.org/two',
        'PRIVATE-HASH-ONE',
      );
      expect(newSource, isNot(newKey));
      final newHash = await revision(
        'PRIVATE-KEY-TWO',
        'https://files.example.org/two',
        'PRIVATE-HASH-TWO',
      );
      expect(newHash, isNot(newSource));
      final presentation = jsonEncode(context.conversation);
      expect(presentation, isNot(contains('PRIVATE-')));
      expect(presentation, isNot(contains('files.example.org')));
    },
    skip: Platform.environment['WADDLE_FLUTTER_RUNTIME_LIBRARY'] == null
        ? 'Build the real Rust runtime and set WADDLE_FLUTTER_RUNTIME_LIBRARY.'
        : false,
  );

  test(
    'real timeline keeps plaintext hashes after raw history eviction',
    () async {
      final transport = NativeRustTransport(FfiRustBindings.load());
      addTearDown(transport.close);
      final context = _Context(realTimeline: transport)..transport = transport;
      const digest = 'CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=';
      context.responses['fetch_room_history'] = {
        'messages': [
          for (var index = 0; index < 1601; index++)
            {
              ..._message('history-$index'),
              if (index == 0)
                'link_previews': [
                  {
                    'original_url': 'https://example.org/old',
                    'title': 'evicted',
                  },
                ],
              if (index == 0)
                'shared_files': [
                  {
                    'url': 'https://files.example.org/old.txt',
                    'name': 'old.txt',
                    'media_type': 'text/plain',
                    'size': 3,
                    'hashes': [
                      {'algo': 'sha-256', 'value_b64': digest},
                    ],
                    'encrypted': null,
                  },
                ],
            },
        ],
        'is_complete': true,
      };
      final runtime = ConversationRuntime(context);
      addTearDown(runtime.dispose);
      await _start(runtime);
      await runtime.invoke('conversation.open', viewArgs);
      await _until(
        () => runtimeRows(context.conversation['messages']).length == 1601,
      );
      final oldest = runtimeRows(
        context.conversation['messages'],
      ).singleWhere((row) => row['id'] == 'history-0');
      expect(runtimeRows(oldest['linkPreviews']), isEmpty);
      await runtime.invoke('conversation.fileOpen', {
        ...viewArgs,
        'targetId': 'history-0',
        'fileId': '0',
      });
      final file = runtimeMap(context.recorded('file.open').single['file']);
      expect(runtimeRows(file['hashes']).single['value_b64'], digest);
    },
    skip: Platform.environment['WADDLE_FLUTTER_RUNTIME_LIBRARY'] == null
        ? 'Build the real Rust runtime and set WADDLE_FLUTTER_RUNTIME_LIBRARY.'
        : false,
  );

  test(
    'notification reply never replaces the active view or publishes read state',
    () async {
      final context = _Context();
      final runtime = ConversationRuntime(context);
      addTearDown(runtime.dispose);
      await runtime.invoke('conversation.open', viewArgs);
      await runtime.notificationReply(
        jid: 'person@example.org',
        group: false,
        body: 'An inline reply',
      );
      expect(context.conversation['viewId'], 'room-view');
      expect(context.recorded('send_displayed'), isEmpty);
      expect(
        runtimeRows(context.persistent['conversationOutbox']).single['jid'],
        'person@example.org',
      );
    },
  );

  test(
    'persist-first notification holds through initial connection and replays only one origin',
    () async {
      final context = _Context();
      final runtime = ConversationRuntime(context);
      addTearDown(runtime.dispose);
      var completed = false;
      final action = runtime
          .notificationReply(
            jid: room,
            group: true,
            body: 'Saved before connection',
            waitForConnection: true,
          )
          .then((_) => completed = true);
      await _until(
        () => runtimeRows(context.persistent['conversationOutbox']).isNotEmpty,
      );
      final id =
          runtimeRows(
                context.persistent['conversationOutbox'],
              ).single['origin_id']
              as String;
      expect(context.recorded('send_groupchat_message'), isEmpty);
      expect(completed, isFalse);
      await _start(runtime);
      await _selfPresence(runtime);
      expect(
        runtimeMap(
          context.recorded('send_groupchat_message').single['options'],
        )['stanza_id'],
        id,
      );
      await runtime.onEvent({
        'type': 'deliveryAcked',
        'data': {'stanza_id': id},
      });
      await runtime.onEvent({
        'type': 'message',
        'data': _message('persist-first-reflection', origin: id, mine: true),
      });
      await action;
      expect(completed, isTrue);
      expect(runtimeRows(context.persistent['conversationOutbox']), isEmpty);
    },
  );

  test(
    'notification reply holds its caller until ACK and trusted own room reflection',
    () async {
      final context = _Context();
      final runtime = ConversationRuntime(context);
      addTearDown(runtime.dispose);
      await _start(runtime);
      await _selfPresence(runtime);
      await runtime.invoke('conversation.open', viewArgs);
      await _flush();
      var completed = false;
      final notification = runtime
          .notificationReply(
            jid: room,
            group: true,
            body: 'Keep the demand lease',
          )
          .then((_) => completed = true);
      await _until(() => context.recorded('send_groupchat_message').isNotEmpty);
      final id =
          runtimeMap(
                context.recorded('send_groupchat_message').single['options'],
              )['stanza_id']
              as String;
      expect(completed, isFalse);
      await runtime.onEvent({
        'type': 'deliveryAcked',
        'data': {'stanza_id': id},
      });
      await _flush();
      expect(completed, isFalse);
      await runtime.onEvent({
        'type': 'message',
        'data': {
          ..._message('confirmed-reply', origin: id, mine: true),
          'body': 'Keep the demand lease',
        },
      });
      await notification;
      expect(completed, isTrue);
      expect(runtimeRows(context.persistent['conversationOutbox']), isEmpty);
      expect(context.conversation['viewId'], 'room-view');
      expect(context.recorded('send_displayed'), isEmpty);
    },
  );

  test(
    'notification delivery deadline reports queued and preserves the same durable origin',
    () async {
      final context = _Context();
      final runtime = ConversationRuntime(
        context,
        notificationSendDeadline: const Duration(milliseconds: 40),
      );
      addTearDown(runtime.dispose);
      await _start(runtime);
      final reply = runtime.notificationReply(
        jid: room,
        group: true,
        body: 'Still queued',
      );
      final assertion = expectLater(
        reply,
        throwsA(
          isA<StateError>().having(
            (error) => error.message,
            'explicit queue status',
            contains('queued for the next connection'),
          ),
        ),
      );
      await _until(() => context.recorded('send_groupchat_message').isNotEmpty);
      final id = runtimeMap(
        context.recorded('send_groupchat_message').single['options'],
      )['stanza_id'];
      await assertion;
      expect(
        runtimeRows(
          context.persistent['conversationOutbox'],
        ).single['origin_id'],
        id,
      );
      expect(context.recorded('send_groupchat_message'), hasLength(1));
    },
  );

  test(
    'outgoing Unicode mentions survive formatting and reply fallback offsets',
    () async {
      final context = _Context();
      context.responses['fetch_room_history'] = {
        'messages': [_message('parent')],
        'is_complete': true,
      };
      final runtime = ConversationRuntime(context);
      addTearDown(runtime.dispose);
      await _start(runtime);
      await runtime.invoke('conversation.open', viewArgs);
      await _flush();
      await runtime.invoke('conversation.send', {
        ...viewArgs,
        'body': '😀 **@alex**',
        'replyTo': 'parent',
        'mentions': [
          {'uri': 'xmpp:alex@example.org', 'begin': 4, 'end': 9},
        ],
      });
      await _flush();
      final sent = context.recorded('send_groupchat_message').single;
      final options = runtimeMap(sent['options']);
      final prefix = '> Message parent\n\n'.runes.length;
      final reference = runtimeRows(options['references']).single;
      expect(reference['begin'], prefix + 2);
      expect(reference['end'], prefix + 7);
      expect(sent['body'], '> Message parent\n\n😀 @alex');
    },
  );
  test(
    'canceling a reply after a storage failure does not quote it in the next send',
    () async {
      final context = _Context();
      context.responses['fetch_room_history'] = {
        'messages': [_message('parent')],
        'is_complete': true,
      };
      final runtime = ConversationRuntime(context);
      addTearDown(runtime.dispose);
      await _start(runtime);
      await runtime.invoke('conversation.open', viewArgs);
      await _flush();
      context.persistenceFails = true;
      await expectLater(
        runtime.invoke('conversation.send', {
          ...viewArgs,
          'body': 'Failed draft',
          'replyTo': 'parent',
        }),
        throwsStateError,
      );
      context.persistenceFails = false;
      await runtime.invoke('conversation.send', {
        ...viewArgs,
        'body': 'A new message',
        'replyTo': null,
      });
      await _flush();
      final sent = context.recorded('send_groupchat_message').single;
      expect(sent['body'], 'A new message');
      expect(runtimeMap(sent['options'])['reply'], isNull);
    },
  );

  test(
    'attachment reply target is captured before the asynchronous picker and cannot follow another draft',
    () async {
      final context = _Context();
      context.responses['fetch_room_history'] = {
        'messages': [_message('selected-parent'), _message('new-parent')],
        'is_complete': true,
      };
      final picker = Completer<dynamic>();
      context.callbacks['file.pickAndUpload'] = (_) => picker.future;
      final runtime = ConversationRuntime(context);
      addTearDown(runtime.dispose);
      await _start(runtime);
      await runtime.invoke('conversation.open', viewArgs);
      await _flush();
      final upload = runtime.invoke('conversation.attach', {
        ...viewArgs,
        'replyTo': 'selected-parent',
      });
      await _flush();
      await runtime.invoke('conversation.send', {
        ...viewArgs,
        'body': 'A different text reply',
        'replyTo': 'new-parent',
      });
      picker.complete({
        'url': 'https://files.example.org/attached.png',
        'name': 'attached.png',
        'media_type': 'image/png',
        'size': 3,
        'disposition': 'attachment',
        'hashes': [],
        'encrypted': null,
      });
      expect(await upload, isTrue);
      await _until(
        () => context.recorded('send_groupchat_message').length == 2,
      );
      final sent = context.recorded('send_groupchat_message');
      final attachment = sent.singleWhere(
        (row) =>
            runtimeRows(runtimeMap(row['options'])['shared_files']).isNotEmpty,
      );
      expect(
        runtimeMap(runtimeMap(attachment['options'])['reply'])['message_id'],
        'selected-parent',
      );
      expect(attachment['body'], '> Message selected-parent\n\n');
      final text = sent.singleWhere(
        (row) =>
            runtimeRows(runtimeMap(row['options'])['shared_files']).isEmpty,
      );
      expect(
        runtimeMap(runtimeMap(text['options'])['reply'])['message_id'],
        'new-parent',
      );
    },
  );

  test(
    'unread divider uses authoritative inbox count without a saved local cursor',
    () async {
      final context = _Context();
      context.responses['fetch_room_history'] = {
        'messages': [
          for (final id in ['read', 'oldest-unread', 'newest-unread'])
            _message(id),
        ],
        'is_complete': true,
      };
      final runtime = ConversationRuntime(context);
      addTearDown(runtime.dispose);
      await _start(runtime);
      await runtime.invoke('conversation.open', viewArgs);
      await _flush();
      context.persistent['conversationUnreadCounts'] = {room: 2};
      runtime.onInboxChanged();
      expect(
        runtimeRows(
          context.conversation['messages'],
        ).where((row) => row['unreadDivider'] == true).single['id'],
        'oldest-unread',
      );
    },
  );

  test(
    'failed correction restores the exact draft even on an RPC exception',
    () async {
      final context = _Context();
      context.responses['fetch_room_history'] = {
        'messages': [_message('own-edit', mine: true)],
        'is_complete': true,
      };
      context.callbacks['send_correction'] = (_) async =>
          throw StateError('Archive connection lost');
      final runtime = ConversationRuntime(context);
      addTearDown(runtime.dispose);
      await _start(runtime);
      await runtime.invoke('conversation.open', viewArgs);
      await _flush();
      await expectLater(
        runtime.invoke('conversation.edit', {
          ...viewArgs,
          'targetId': 'own-edit',
          'body': '**Keep** 😀 draft',
        }),
        throwsStateError,
      );
      final composer = context.emissions
          .where((entry) => entry.$1 == 'composer')
          .last
          .$2;
      expect(composer['mode'], 'editing');
      expect(composer['body'], '**Keep** 😀 draft');
      expect(composer['attempt'], 1);
      expect(
        context.recorded('send_correction').single['new_body'],
        'Keep 😀 draft',
      );
    },
  );

  test(
    'notification mode is durable only after a successful typed server outcome',
    () async {
      final context = _Context();
      final runtime = ConversationRuntime(context);
      addTearDown(runtime.dispose);
      await runtime.invoke('conversation.open', viewArgs);
      context.responses['set_room_notification_mode'] = {
        'type': 'node_config_mismatch',
      };
      await expectLater(
        runtime.invoke('conversation.notify', {...viewArgs, 'mode': 'never'}),
        throwsStateError,
      );
      expect(context.persistent['notificationModes'], isNull);
      context.responses['set_room_notification_mode'] = {'type': 'ok'};
      await runtime.invoke('conversation.notify', {
        ...viewArgs,
        'mode': 'on_mention',
      });
      expect(
        runtimeMap(context.persistent['notificationModes'])[room],
        'on_mention',
      );
    },
  );

  test(
    'a pending send does not hide another archived row with its origin',
    () async {
      final transport = NativeRustTransport(FfiRustBindings.load());
      addTearDown(transport.close);
      final context = _Context(realTimeline: transport)..transport = transport;
      context.responses['fetch_room_history'] = {
        'messages': [
          {
            ..._message('bootstrap'),
            'reaction_target_id': 'not-loaded',
            'reaction_emojis': ['👍'],
          },
        ],
        'is_complete': false,
        'first_id': 'bootstrap',
      };
      final runtime = ConversationRuntime(context);
      addTearDown(runtime.dispose);
      await _start(runtime);
      await _selfPresence(runtime);
      await runtime.invoke('conversation.open', viewArgs);
      await _until(() => context.recorded('fetch_room_pins').isNotEmpty);
      final localFile = {
        'url': 'https://files.example.org/local.png',
        'name': 'local.png',
        'media_type': 'image/png',
        'size': 3,
        'hashes': <dynamic>[],
        'encrypted': null,
      };
      context.responses['file.pickAndUpload'] = localFile;
      await runtime.invoke('conversation.attach', viewArgs);
      await _until(() => context.recorded('send_groupchat_message').isNotEmpty);
      final id =
          runtimeMap(
                context.recorded('send_groupchat_message').single['options'],
              )['stanza_id']
              as String;
      await runtime.onEvent({
        'type': 'message',
        'data': {
          ..._message('confirmed-own', origin: id, mine: true),
          'body': '',
          'shared_files': [localFile],
        },
      });
      context.responses['fetch_room_history'] = {
        'messages': [
          {
            ..._message('other-archive', origin: id, mine: true),
            'mam_id': 'other-archive',
            'body': 'Another occupant used this nickname',
            'shared_files': [
              {
                'url': 'https://files.example.org/other.png',
                'name': 'other.png',
                'media_type': 'image/png',
                'size': 3,
                'hashes': <dynamic>[],
                'encrypted': null,
              },
            ],
          },
        ],
        'is_complete': true,
      };
      await runtime.invoke('conversation.history', viewArgs);
      final beforeAck = runtimeRows(context.conversation['messages']);
      expect(beforeAck.where((row) => row['pending'] == true), hasLength(1));
      final other = beforeAck.singleWhere(
        (row) => row['id'] == 'other-archive',
      );
      expect(runtimeRows(other['files']).single['name'], 'other.png');
      await runtime.invoke('conversation.fileBytes', {
        ...viewArgs,
        'targetId': 'other-archive',
        'fileId': '0',
      });
      expect(
        runtimeMap(context.recorded('file.bytes').last['file'])['name'],
        'other.png',
      );
      await runtime.onEvent({
        'type': 'deliveryAcked',
        'data': {'stanza_id': id},
      });
      await _until(
        () => runtimeRows(context.persistent['conversationOutbox']).isEmpty,
      );
      final afterAck = runtimeRows(context.conversation['messages']);
      expect(
        afterAck.where((row) => row['id'] == 'confirmed-own'),
        hasLength(1),
      );
      expect(
        afterAck.where((row) => row['id'] == 'other-archive'),
        hasLength(1),
      );
    },
    skip: Platform.environment['WADDLE_FLUTTER_RUNTIME_LIBRARY'] == null
        ? 'Build the real Rust runtime and set WADDLE_FLUTTER_RUNTIME_LIBRARY.'
        : false,
  );

  test(
    'real Rust foreign origin collision cannot settle an acknowledged local send',
    () async {
      final transport = NativeRustTransport(FfiRustBindings.load());
      addTearDown(transport.close);
      final context = _Context(realTimeline: transport)..transport = transport;
      context.responses['fetch_room_history'] = {
        'messages': [
          {
            ..._message('bootstrap'),
            'reaction_target_id': 'not-loaded',
            'reaction_emojis': ['👍'],
          },
        ],
        'is_complete': false,
        'first_id': 'bootstrap',
      };
      final runtime = ConversationRuntime(context);
      addTearDown(runtime.dispose);
      await _start(runtime);
      await runtime.invoke('conversation.open', viewArgs);
      await _until(() => context.recorded('fetch_room_pins').isNotEmpty);
      await runtime.onEvent({
        'type': 'presence',
        'data': {
          'from': '$room/me',
          'muc_jid': own,
          'muc_status_codes': [110],
          'muc_affiliation': 'member',
        },
      });
      final localFile = {
        'url': 'https://private.example.org/secret.bin',
        'name': 'private.png',
        'media_type': 'image/png',
        'size': 3,
        'hashes': <dynamic>[],
        'encrypted': {
          'cipher': 'urn:xmpp:ciphers:aes-256-gcm-nopadding:0',
          'key_b64': 'PRIVATE-KEY',
          'iv_b64': 'PRIVATE-IV',
          'hashes': <dynamic>[],
          'sources': ['https://private.example.org/secret.bin'],
        },
      };
      context.responses['file.pickAndUpload'] = localFile;
      await runtime.invoke('conversation.attach', viewArgs);
      await _until(() => context.recorded('send_groupchat_message').isNotEmpty);
      final id =
          runtimeMap(
                context.recorded('send_groupchat_message').single['options'],
              )['stanza_id']
              as String;
      await runtime.onEvent({
        'type': 'deliveryAcked',
        'data': {'stanza_id': id},
      });
      await runtime.onEvent({
        'type': 'message',
        'data': {
          ..._message('foreign-reflection', origin: id),
          'from': '$room/attacker',
          'body': 'An attacker reused a visible origin ID',
          'shared_files': [
            {
              'url': 'https://foreign.example.org/public.txt',
              'name': 'public.txt',
              'media_type': 'text/plain',
              'size': 3,
              'hashes': <dynamic>[],
              'encrypted': null,
            },
          ],
        },
      });
      expect(
        runtimeRows(context.persistent['conversationOutbox']),
        hasLength(1),
      );
      expect(
        runtimeRows(
          context.conversation['messages'],
        ).where((row) => row['pending'] == true),
        hasLength(1),
      );
      final projected = runtimeRows(context.conversation['messages']);
      final foreign = projected.singleWhere(
        (row) => row['id'] == 'foreign-reflection',
      );
      final pending = projected.singleWhere((row) => row['pending'] == true);
      expect(runtimeRows(foreign['files']).single['name'], 'public.txt');
      expect(runtimeRows(pending['files']).single['name'], 'private.png');
      expect(jsonEncode(foreign), isNot(contains('private.example.org')));
      expect(jsonEncode(foreign), isNot(contains('PRIVATE-KEY')));
      await runtime.invoke('conversation.fileBytes', {
        ...viewArgs,
        'targetId': 'foreign-reflection',
        'fileId': '0',
      });
      final opened = runtimeMap(context.recorded('file.bytes').last['file']);
      expect(opened['name'], 'public.txt');
      expect(opened['url'], 'https://foreign.example.org/public.txt');
      expect(jsonEncode(opened), isNot(contains('PRIVATE-KEY')));
      await expectLater(
        runtime.invoke('conversation.fileBytes', {
          ...viewArgs,
          'targetId': 'foreign-reflection',
          'fileId': '1',
        }),
        throwsStateError,
      );
      final archivedImposter = {
        ..._message('archive-imposter', origin: id, mine: true),
        'mam_id': 'archive-imposter',
        'body': 'Reused nickname',
        'shared_files': [
          {
            'url': 'https://foreign.example.org/archive.txt',
            'name': 'archive.txt',
            'media_type': 'text/plain',
            'size': 3,
            'hashes': <dynamic>[],
            'encrypted': null,
          },
        ],
      };
      context.responses['fetch_room_history'] = {
        'messages': [archivedImposter],
        'is_complete': false,
        'first_id': 'archive-imposter',
      };
      await runtime.invoke('conversation.history', viewArgs);
      expect(
        runtimeRows(context.persistent['conversationOutbox']),
        hasLength(1),
      );
      expect(
        runtimeRows(
          context.conversation['messages'],
        ).where((row) => row['pending'] == true),
        hasLength(1),
      );
      await runtime.onEvent({
        'type': 'message',
        'data': {
          ..._message('genuine-reflection', origin: id, mine: true),
          'body': '',
          'shared_files': [localFile],
        },
      });
      await _until(
        () => runtimeRows(context.persistent['conversationOutbox']).isEmpty,
      );
      final rows = runtimeRows(context.conversation['messages']);
      final ownRow = rows.where((row) => row['mine'] == true).single;
      expect(runtimeRows(ownRow['files']).single['name'], 'private.png');
      expect(
        rows.where((row) => row['mine'] != true).single['body'],
        'An attacker reused a visible origin ID',
      );
      context.responses['fetch_room_history'] = {
        'messages': [archivedImposter],
        'is_complete': false,
        'first_id': 'archive-imposter',
      };
      await runtime.invoke('conversation.history', viewArgs);
      final imposter = runtimeRows(
        context.conversation['messages'],
      ).singleWhere((row) => row['id'] == 'archive-imposter');
      expect(runtimeRows(imposter['files']).single['name'], 'archive.txt');
      expect(jsonEncode(imposter), isNot(contains('private.example.org')));
      await runtime.invoke('conversation.fileBytes', {
        ...viewArgs,
        'targetId': 'archive-imposter',
        'fileId': '0',
      });
      final archiveFile = runtimeMap(
        context.recorded('file.bytes').last['file'],
      );
      expect(archiveFile['url'], 'https://foreign.example.org/archive.txt');
      expect(jsonEncode(archiveFile), isNot(contains('PRIVATE-KEY')));
      await runtime.invoke('conversation.fileBytes', {
        ...viewArgs,
        'targetId': 'genuine-reflection',
        'fileId': '0',
      });
      final genuineFile = runtimeMap(
        context.recorded('file.bytes').last['file'],
      );
      expect(genuineFile['url'], 'https://private.example.org/secret.bin');
      expect(runtimeMap(genuineFile['encrypted'])['key_b64'], 'PRIVATE-KEY');
      final otherFile = {
        ...localFile,
        'url': 'https://other.example.org/other.bin',
        'name': 'other.png',
        'encrypted': {
          ...runtimeMap(localFile['encrypted']),
          'key_b64': 'OTHER-KEY',
          'sources': ['https://other.example.org/other.bin'],
        },
      };
      context.responses['fetch_room_history'] = {
        'messages': [
          {
            ..._message('other-own-archive', origin: id, mine: true),
            'mam_id': 'other-own-archive',
            'author_real_jid': own,
            'body': '',
            'shared_files': [otherFile],
          },
        ],
        'is_complete': true,
      };
      await runtime.invoke('conversation.history', viewArgs);
      final ownRows = runtimeRows(context.conversation['messages']);
      final other = ownRows.singleWhere(
        (row) => row['id'] == 'other-own-archive',
      );
      final genuine = ownRows.singleWhere(
        (row) => row['id'] == 'genuine-reflection',
      );
      expect(runtimeRows(other['files']).single['name'], 'other.png');
      expect(
        runtimeRows(other['files']).single['contentRevision'],
        isNot(runtimeRows(genuine['files']).single['contentRevision']),
      );
      await runtime.invoke('conversation.fileBytes', {
        ...viewArgs,
        'targetId': 'other-own-archive',
        'fileId': '0',
      });
      final openedOther = runtimeMap(
        context.recorded('file.bytes').last['file'],
      );
      expect(openedOther['url'], 'https://other.example.org/other.bin');
      expect(runtimeMap(openedOther['encrypted'])['key_b64'], 'OTHER-KEY');
      await runtime.invoke('conversation.fileBytes', {
        ...viewArgs,
        'targetId': 'genuine-reflection',
        'fileId': '0',
      });
      expect(
        runtimeMap(context.recorded('file.bytes').last['file'])['url'],
        'https://private.example.org/secret.bin',
      );
    },
    skip: Platform.environment['WADDLE_FLUTTER_RUNTIME_LIBRARY'] == null
        ? 'Build the real Rust runtime and set WADDLE_FLUTTER_RUNTIME_LIBRARY.'
        : false,
  );

  test(
    'real Rust boundary rejects spoofed actions and keeps attachment secrets private',
    () async {
      final transport = NativeRustTransport(FfiRustBindings.load());
      addTearDown(transport.close);
      final context = _Context(realTimeline: transport)..transport = transport;
      final runtime = ConversationRuntime(context);
      addTearDown(runtime.dispose);
      await _start(runtime);
      await runtime.invoke('conversation.open', viewArgs);
      await _until(() => context.recorded('fetch_room_pins').isNotEmpty);
      final source = {
        ..._message('untrusted-id'),
        'id': 'alex-author-id',
        'stanza_id_by': 'attacker.example.org',
        'stanza_ids': [
          {'id': 'untrusted-first', 'by': 'attacker.example.org'},
          {'id': 'room-anchor', 'by': room},
        ],
        'shared_files': [
          {
            'url': 'https://files.example.org/secret.bin',
            'name': 'private.png',
            'media_type': 'image/png',
            'size': 32,
            'disposition': 'attachment',
            'encrypted': {
              'cipher': 'urn:xmpp:ciphers:aes-256-gcm-nopadding',
              'key_b64': 'SECRET-KEY',
              'iv_b64': 'SECRET-IV',
              'hashes': [
                {'algo': 'sha-256', 'value_b64': 'SECRET-HASH'},
              ],
              'sources': ['https://files.example.org/secret.bin'],
            },
          },
        ],
      };
      await runtime.onEvent({'type': 'message', 'data': source});
      final initial = runtimeRows(context.conversation['messages']).single;
      expect(initial['id'], 'room-anchor');
      expect(initial['body'], 'Message untrusted-id');
      await runtime.invoke('conversation.read', {
        ...viewArgs,
        'atNewestEdge': true,
      });
      await _until(() => context.recorded('send_displayed').isNotEmpty);
      expect(
        context.recorded('send_displayed').single['stanza_id'],
        'room-anchor',
      );
      await _until(
        () =>
            runtimeMap(context.persistent['conversationReadCursors'])[room] !=
            null,
      );
      final projection = jsonEncode(context.conversation);
      expect(projection, isNot(contains('SECRET-')));
      expect(projection, isNot(contains('secret.bin')));
      expect(runtimeRows(initial['files']).single['encrypted'], isTrue);
      await runtime.onEvent({
        'type': 'message',
        'data': {
          ..._message('spoof-edit'),
          'from': '$room/attacker',
          'replaces_id': 'alex-author-id',
          'body': 'Forged correction',
        },
      });
      expect(
        runtimeRows(context.conversation['messages']).single['body'],
        'Message untrusted-id',
      );
      await runtime.invoke('conversation.reaction', {
        ...viewArgs,
        'targetId': 'room-anchor',
        'emoji': '👍',
      });
      expect(
        context.recorded('send_reaction').single['target_stanza_id'],
        'room-anchor',
      );
      await runtime.onEvent({
        'type': 'message',
        'data': {
          ..._message('valid-edit'),
          'replaces_id': 'alex-author-id',
          'body': 'Actual correction',
        },
      });
      expect(
        runtimeRows(context.conversation['messages']).single['body'],
        'Actual correction',
      );
      await runtime.invoke('conversation.send', {
        ...viewArgs,
        'body': '😀 **@alex**',
        'replyTo': 'room-anchor',
        'mentions': [
          {'uri': 'xmpp:alex@example.org', 'begin': 4, 'end': 9},
        ],
      });
      await _until(() => context.recorded('send_groupchat_message').isNotEmpty);
      final payload = context.recorded('send_groupchat_message').single;
      expect(payload['body'], '> Actual correction\n\n😀 @alex');
      // A real native deserialize must accept generated nested reply/markup/
      // reference options, then fail only because this handle has no socket.
      await expectLater(
        transport.rpc('send_groupchat_message', payload),
        throwsA(
          isA<RustTransportException>().having(
            (error) => error.code,
            'typed RPC schema',
            'not_connected',
          ),
        ),
      );
      context.responses['file.pickAndUpload'] = {
        'url': 'https://files.example.org/attached.png',
        'name': 'attached.png',
        'media_type': 'image/png',
        'size': 3,
        'disposition': 'attachment',
        'hashes': [],
        'encrypted': null,
      };
      expect(
        await runtime.invoke('conversation.attach', {
          ...viewArgs,
          'replyTo': 'room-anchor',
        }),
        isTrue,
      );
      await _until(
        () => context.recorded('send_groupchat_message').length == 2,
      );
      final attachment = context.recorded('send_groupchat_message').last;
      expect(attachment['body'], '> Actual correction\n\n');
      expect(
        runtimeMap(runtimeMap(attachment['options'])['reply'])['message_id'],
        'room-anchor',
      );
      await expectLater(
        transport.rpc('send_groupchat_message', attachment),
        throwsA(
          isA<RustTransportException>().having(
            (error) => error.code,
            'attachment reply typed RPC schema',
            'not_connected',
          ),
        ),
      );
    },
    skip: Platform.environment['WADDLE_FLUTTER_RUNTIME_LIBRARY'] == null
        ? 'Build the real Rust runtime and set WADDLE_FLUTTER_RUNTIME_LIBRARY.'
        : false,
  );
}

Future<void> _until(bool Function() condition) async {
  final deadline = DateTime.now().add(const Duration(seconds: 5));
  while (!condition()) {
    if (DateTime.now().isAfter(deadline)) {
      throw TimeoutException('Runtime did not settle');
    }
    await Future<void>.delayed(const Duration(milliseconds: 10));
  }
}

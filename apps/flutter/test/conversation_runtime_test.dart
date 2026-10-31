import 'dart:async';
import 'dart:convert';
import 'dart:io';

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
    'real Rust foreign origin collision cannot settle an acknowledged local send',
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
        'data': {
          'from': '$room/me',
          'muc_jid': own,
          'muc_status_codes': [110],
          'muc_affiliation': 'member',
        },
      });
      await runtime.invoke('conversation.send', {
        ...viewArgs,
        'body': 'Genuine outbound',
      });
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
      await runtime.onEvent({
        'type': 'message',
        'data': {
          ..._message('genuine-reflection', origin: id, mine: true),
          'body': 'Genuine outbound',
        },
      });
      await _until(
        () => runtimeRows(context.persistent['conversationOutbox']).isEmpty,
      );
      final rows = runtimeRows(context.conversation['messages']);
      expect(
        rows.where((row) => row['mine'] == true).single['body'],
        'Genuine outbound',
      );
      expect(
        rows.where((row) => row['mine'] != true).single['body'],
        'An attacker reused a visible origin ID',
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

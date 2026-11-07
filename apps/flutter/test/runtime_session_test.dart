import 'dart:async';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_flutter/runtime/context.dart';
import 'package:waddle_flutter/runtime/session_auth.dart';
import 'package:waddle_flutter/runtime/session_runtime.dart';
import 'package:waddle_flutter/runtime/storage.dart';
import 'package:waddle_flutter/transport/native_bindings.dart';
import 'package:waddle_flutter/transport/transport.dart';
import 'package:waddle_flutter/transport/transport_native.dart';

class _Storage implements RuntimeStorage {
  final values = <String, String>{};
  @override
  Future<String?> read(String key) async => values[key];
  @override
  Future<void> write(String key, String? value) async {
    if (value == null) {
      values.remove(key);
    } else {
      values[key] = value;
    }
  }
}

class _DelayedStorage extends _Storage {
  Completer<void>? accountWrite;
  @override
  Future<void> write(String key, String? value) async {
    if (key.startsWith('account:') && accountWrite != null) {
      await accountWrite!.future;
    }
    await super.write(key, value);
  }
}

Map<String, dynamic> _credentials(String id, String owner) => {
  'session_id': id,
  'jid': owner,
  'username': owner.split('@').first,
  'xmpp_localpart': owner.split('@').first,
  'is_expired': false,
  'xmpp_websocket_url': 'wss://xmpp.example.org/xmpp-websocket',
};

class _Auth implements AuthGateway {
  Future<Map<String, dynamic>?> Function(String)? readSession;
  Future<Map<String, dynamic>> Function(String)? poll;
  int sessionReads = 0;
  final revoked = <String>[];
  @override
  Future<List<Map<String, dynamic>>> providers() async => [
    {'id': 'community', 'display_name': 'Community'},
  ];
  @override
  Future<Map<String, dynamic>> startDevice(String provider) async => {
    'device_code': 'private-device-code',
    'user_code': 'ABCD-EFGH',
    'verification_uri': 'https://example.org/approve',
    'interval': 1,
  };
  @override
  Future<Map<String, dynamic>> pollDevice(String code) async =>
      poll != null ? poll!(code) : {'status': 'pending'};
  @override
  Future<Map<String, dynamic>?> session(String id) async {
    sessionReads++;
    return readSession != null
        ? readSession!(id)
        : _credentials(id, 'me@example.org');
  }

  @override
  Future<void> logout(String id) async {
    revoked.add(id);
  }
}

class _Transport implements RustTransport {
  _Transport({this.timeline});

  final controller = StreamController<Map<String, dynamic>>.broadcast();
  final calls = <(String, Map<String, dynamic>)>[];
  final localRows = <Map<String, dynamic>>[];
  final NativeRustTransport? timeline;
  final responses = <String, dynamic>{};
  Future<dynamic> Function(String, Map<String, dynamic>)? handler;
  bool closed = false;
  int closeCalls = 0;
  Future<void> Function()? closeHandler;
  @override
  Stream<Map<String, dynamic>> get events => controller.stream;
  @override
  Future<dynamic> rpc(
    String method, [
    Map<String, dynamic> args = const {},
  ]) async {
    calls.add((method, Map.of(args)));
    if (handler != null) {
      return handler!(method, args);
    }
    if (method.startsWith('timeline_') && timeline != null) {
      return timeline!.rpc(method, args);
    }
    if (responses.containsKey(method)) return responses[method];
    if (method == 'timeline_local_echo') {
      localRows.add({
        'id': args['origin_id'],
        'origin_id': args['origin_id'],
        'own': true,
        'local_echo': true,
        'identity_kind': 'origin',
      });
    }
    if (method.startsWith('timeline_')) {
      return {'items': localRows};
    }
    return switch (method) {
      'discover_topology' => {'channels': <dynamic>[]},
      'fetch_inbox' => {'conversations': <dynamic>[]},
      'is_community_owner' => false,
      'send_chat_message' || 'send_groupchat_message' => {
        'type': 'sent',
        'stanza_id': runtimeMap(args['options'])['stanza_id'],
      },
      'fetch_user_bookmarks' || 'fetch_dm_bookmarks' => <dynamic>[],
      _ => null,
    };
  }

  @override
  Future<void> close() async {
    closeCalls++;
    await closeHandler?.call();
    await timeline?.close();
    closed = true;
  }
}

Map<String, dynamic> _archive(
  String origin,
  String body,
  String archiveId, {
  bool peer = false,
  List<Map<String, dynamic>> files = const [],
}) => {
  'id': origin,
  'origin_id': origin,
  'from': peer ? 'friend@example.org/phone' : 'me@example.org/mobile',
  'to': peer ? 'me@example.org' : 'friend@example.org',
  'message_type': 'chat',
  'body': body,
  'mam_id': archiveId,
  'timestamp': '2026-10-31T09:00:00Z',
  'subject': null,
  'thread': null,
  'parent_thread_id': null,
  'reply_to_id': null,
  'reply_to_sender': null,
  'reply_fallback_start': null,
  'reply_fallback_end': null,
  'markup_spans': <dynamic>[],
  'references': <dynamic>[],
  'muc_user': false,
  'is_sticker': false,
  'is_retracted': false,
  'replaces_id': null,
  'retracts_id': null,
  'moderation_target_id': null,
  'reaction_target_id': null,
  'call_thread': null,
  'call_thread_ended': null,
  'call_event': null,
  'safety_scores': null,
  'link_previews': <dynamic>[],
  'shared_files': files,
};

Future<void> _flush() async {
  for (var i = 0; i < 8; i++) {
    await Future<void>.delayed(Duration.zero);
  }
}

Future<void> _until(bool Function() condition, String operation) async {
  for (var i = 0; i < 500; i++) {
    if (condition()) return;
    await Future<void>.delayed(const Duration(milliseconds: 5));
  }
  fail('The runtime did not settle: $operation');
}

void main() {
  test(
    'hidden push delivery pauses the socket and foreground resumes a fresh handle',
    () async {
      final storage = _Storage()..values['session'] = 'private-token';
      final handles = <_Transport>[];
      final runtime = SessionRuntime(
        storage: storage,
        auth: _Auth(),
        initiallyVisible: false,
        transportFactory: () async {
          final handle = _Transport();
          handles.add(handle);
          return handle;
        },
        platformInvoker: (_, _) async => null,
      );
      addTearDown(runtime.dispose);
      await runtime.start();
      await _flush();
      expect(
        handles,
        hasLength(1),
      ); // Unresolved delivery conservatively connects.
      await runtime.conversations.notificationReply(
        jid: 'friend@example.org',
        group: false,
        body: 'Retained offline reply',
      );
      await runtime.effectiveBackgroundDemand(false, pushEnabled: true);
      expect(handles.single.closed, true);
      expect(runtime.transport, isNull);
      expect(runtime.session['state'], 'ready');
      expect(storage.values['session'], 'private-token');
      expect(
        storage.values['account:me@example.org'],
        contains('Retained offline reply'),
      );
      expect(
        runtimeRows(runtime.persistent['conversationOutbox']),
        hasLength(1),
      );
      await runtime.onVisibility(true);
      expect(handles, hasLength(2));
      expect(handles.last.closed, false);
      expect(
        handles.last.calls
            .singleWhere((call) => call.$1 == 'connect')
            .$2['resumable'],
        false,
      );
    },
  );

  test(
    'background reply is persisted before ready and keeps its lease until ACK',
    () async {
      final storage = _Storage()..values['session'] = 'private-token';
      final handles = <_Transport>[];
      final runtime = SessionRuntime(
        storage: storage,
        auth: _Auth(),
        initiallyVisible: false,
        transportFactory: () async {
          final handle = _Transport();
          handles.add(handle);
          return handle;
        },
        platformInvoker: (_, _) async => null,
      );
      addTearDown(runtime.dispose);
      await runtime.start();
      await _flush();
      await runtime.effectiveBackgroundDemand(false, pushEnabled: true);
      var completed = false;
      final reply = runtime
          .withConnectionDemand(
            () => runtime.conversations.notificationReply(
              jid: 'friend@example.org',
              group: false,
              body: 'Persist me before connecting',
              waitForConnection: true,
            ),
            waitForConnection: false,
          )
          .then((_) => completed = true);
      await _flush();
      expect(handles, hasLength(2));
      final active = handles.last;
      final row = runtimeRows(runtime.persistent['conversationOutbox']).single;
      expect(
        storage.values['account:me@example.org'],
        contains('Persist me before connecting'),
      );
      expect(
        active.calls.where((call) => call.$1 == 'send_chat_message'),
        isEmpty,
      );
      expect(completed, false);
      active.controller.add({'type': 'connected', 'data': null});
      await _flush();
      expect(
        active.calls.where((call) => call.$1 == 'send_chat_message'),
        hasLength(1),
      );
      expect(completed, false);
      expect(active.closed, false);
      active.controller.add({
        'type': 'deliveryAcked',
        'data': row['origin_id'],
      });
      await reply;
      expect(completed, true);
      expect(active.closed, true);
      expect(runtime.transport, isNull);
      expect(runtimeRows(runtime.persistent['conversationOutbox']), isEmpty);
      expect(
        storage.values['account:me@example.org'],
        isNot(contains('Persist me before connecting')),
      );

      await runtime.onVisibility(true);
      expect(handles, hasLength(3));
      final resumed = handles.last;
      resumed.controller.add({'type': 'connected', 'data': null});
      await _flush();
      expect(
        resumed.calls.where((call) => call.$1 == 'send_chat_message'),
        isEmpty,
      );
      expect(runtimeRows(runtime.persistent['conversationOutbox']), isEmpty);
      await runtime.dispose();
      final restartedTransport = _Transport();
      final restarted = SessionRuntime(
        storage: storage,
        auth: _Auth(),
        transportFactory: () async => restartedTransport,
        platformInvoker: (_, _) async => null,
      );
      addTearDown(restarted.dispose);
      await restarted.start();
      restartedTransport.controller.add({'type': 'connected', 'data': null});
      await _flush();
      expect(
        restartedTransport.calls.where(
          (call) => call.$1 == 'send_chat_message',
        ),
        isEmpty,
      );
      expect(runtimeRows(restarted.persistent['conversationOutbox']), isEmpty);
    },
  );

  for (final scenario in [
    'ordinary',
    'different archive',
    'equal archive ID',
    'attachment conflict',
    'thread conflict',
    'reply conflict',
    'markup conflict',
  ]) {
    test(
      'an acknowledged DM preserves origin ownership: $scenario',
      () async {
        final storage = _Storage()..values['session'] = 'private-token';
        final active = _Transport(
          timeline: NativeRustTransport(
            FfiRustBindings.load(),
            pollInterval: const Duration(milliseconds: 1),
          ),
        );
        final openedFiles = <Map<String, dynamic>>[];
        final runtime = SessionRuntime(
          storage: storage,
          auth: _Auth(),
          transportFactory: () async => active,
          platformInvoker: (method, args) async {
            if (method == 'file.bytes') {
              openedFiles.add(runtimeMap(args['file']));
            }
            return null;
          },
        );
        final events = <Map<String, dynamic>>[];
        final subscription = runtime.events.listen(events.add);
        addTearDown(subscription.cancel);
        addTearDown(runtime.dispose);
        await runtime.start();
        await _until(
          () => active.calls.any((call) => call.$1 == 'connect'),
          'connect',
        );
        active.controller.add({'type': 'connected', 'data': null});
        await _until(
          () => active.calls.any((call) => call.$1 == 'discover_topology'),
          'initialization',
        );
        final reply = runtime.conversations.notificationReply(
          jid: 'friend@example.org',
          group: false,
          body: 'My archived direct reply',
          waitForConnection: true,
        );
        await _until(
          () => active.calls.any((call) => call.$1 == 'send_chat_message'),
          'direct send',
        );
        final id =
            runtimeRows(
                  runtime.persistent['conversationOutbox'],
                ).single['origin_id']
                as String;
        active.controller.add({'type': 'deliveryAcked', 'data': id});
        await reply.timeout(const Duration(seconds: 3));
        expect(runtimeRows(runtime.persistent['conversationOutbox']), isEmpty);
        expect(storage.values['account:me@example.org'], isNot(contains(id)));

        active.responses['fetch_dm_history'] = {
          'messages': [
            _archive(
              scenario == 'ordinary' ? 'peer-origin' : id,
              'Peer reused the origin',
              'peer-archive-id',
              peer: true,
            ),
          ],
          'is_complete': false,
          'first_id': 'peer-archive-id',
        };
        const view = {
          'jid': 'friend@example.org',
          'group': false,
          'viewId': 'direct',
          'atNewestEdge': false,
        };
        await runtime.conversations.invoke('conversation.open', view);
        await _until(
          () => events.any(
            (event) =>
                event['topic'] == 'conversation' &&
                runtimeMap(event['data'])['loadingOlder'] == false &&
                runtimeRows(
                  runtimeMap(event['data'])['messages'],
                ).any((row) => row['body'] == 'Peer reused the origin'),
          ),
          'peer archive',
        );
        List<Map<String, dynamic>> rows() => runtimeRows(
          runtimeMap(
            events.lastWhere(
              (event) => event['topic'] == 'conversation',
            )['data'],
          )['messages'],
        );
        expect(
          rows().map((row) => row['body']),
          contains('My archived direct reply'),
        );
        final local = rows().singleWhere(
          (row) => row['body'] == 'My archived direct reply',
        );
        expect(local['pending'], isFalse);
        expect(local['id'], id);
        expect(runtimeRows(local['files']), isEmpty);

        final collision = scenario != 'ordinary';
        final hasFile = [
          'different archive',
          'equal archive ID',
          'attachment conflict',
        ].contains(scenario);
        final differentBody = [
          'different archive',
          'equal archive ID',
        ].contains(scenario);
        final archiveId = scenario == 'equal archive ID'
            ? id
            : 'own-archive-id';
        final file = {
          'url': 'https://files.example.org/other.png',
          'name': 'other.png',
          'media_type': 'image/png',
          'size': 3,
          'width': null,
          'height': null,
          'desc': null,
          'disposition': 'attachment',
          'hashes': <dynamic>[],
          'encrypted': null,
        };
        active.responses['fetch_dm_history'] = {
          'messages': [
            {
              ..._archive(
                id,
                differentBody
                    ? 'A different message reused the origin'
                    : 'My archived direct reply',
                archiveId,
                files: hasFile ? [file] : [],
              ),
              if (scenario == 'thread conflict')
                'parent_thread_id': 'other-parent',
              if (scenario == 'reply conflict') 'reply_to_id': 'other-target',
              if (scenario == 'markup conflict')
                'markup_spans': [
                  {'span_type': 'bold', 'start': 0, 'end': 2, 'uri': null},
                ],
            },
          ],
          'is_complete': false,
          'first_id': archiveId,
        };
        await runtime.conversations.invoke('conversation.history', view);
        await _flush();
        expect(runtimeRows(runtime.persistent['conversationOutbox']), isEmpty);
        final current = rows();
        expect(
          current.where((row) => row['id'] == id),
          hasLength(scenario == 'equal archive ID' ? 2 : 1),
        );
        if (collision) {
          final other = current.singleWhere(
            (row) =>
                row['presentationId'] != local['presentationId'] &&
                row['id'] == archiveId,
          );
          expect(runtimeRows(other['files']), hasLength(hasFile ? 1 : 0));
          if (hasFile) {
            expect(runtimeRows(other['files']).single['name'], 'other.png');
          }
          expect(other['presentationId'], isNot(local['presentationId']));
          if (scenario == 'equal archive ID') {
            await expectLater(
              runtime.conversations.invoke('conversation.fileBytes', {
                ...view,
                'targetId': id,
                'fileId': '0',
              }),
              throwsStateError,
            );
            await expectLater(
              runtime.conversations.invoke('conversation.reply', {
                ...view,
                'targetId': id,
              }),
              throwsStateError,
            );
            expect(openedFiles, isEmpty);
          } else if (hasFile) {
            await runtime.conversations.invoke('conversation.fileBytes', {
              ...view,
              'targetId': archiveId,
              'fileId': '0',
            });
            expect(openedFiles.single['name'], 'other.png');
            await expectLater(
              runtime.conversations.invoke('conversation.fileBytes', {
                ...view,
                'targetId': id,
                'fileId': '0',
              }),
              throwsStateError,
            );
          }
          if (scenario == 'different archive') {
            for (final method in [
              'conversation.retract',
              'conversation.edit',
              'conversation.reply',
              'conversation.reaction',
            ]) {
              await expectLater(
                runtime.conversations.invoke(method, {
                  ...view,
                  'targetId': archiveId,
                  if (method == 'conversation.edit') 'body': 'changed',
                  if (method == 'conversation.reaction') 'emoji': '👍',
                }),
                throwsStateError,
              );
            }
            expect(
              active.calls.where(
                (call) => [
                  'send_retraction',
                  'send_correction',
                  'send_reaction',
                ].contains(call.$1),
              ),
              isEmpty,
            );
            final thread = {
              ...view,
              'viewId': 'ambiguous-thread',
              'threadId': id,
            };
            await runtime.conversations.invoke('conversation.open', thread);
            await expectLater(
              runtime.conversations.invoke('conversation.send', {
                ...thread,
                'body': 'Do not send into an ambiguous thread',
              }),
              throwsStateError,
            );
            expect(
              active.calls.where((call) => call.$1 == 'send_chat_message'),
              hasLength(1),
            );
            await runtime.conversations.onEvent({
              'type': 'messageRejected',
              'data': {
                'stanza_id': id,
                'from': 'mallory@example.org',
                'to': 'me@example.org',
              },
            });
            expect(
              runtimeRows(runtime.persistent['conversationOutbox']),
              isEmpty,
            );
            await runtime.conversations.onEvent({
              'type': 'messageRejected',
              'data': {
                'stanza_id': id,
                'from': 'friend@example.org',
                'to': 'me@example.org',
              },
            });
            final rejected = runtimeRows(
              runtime.persistent['conversationOutbox'],
            ).single;
            expect(rejected['failed'], isTrue);
            expect(rejected['rejected'], isTrue);
          }
        } else {
          expect(current.where((row) => row['id'] == archiveId), isEmpty);
          await runtime.conversations.invoke('conversation.reply', {
            ...view,
            'targetId': id,
          });
          await runtime.conversations.invoke('conversation.send', {
            ...view,
            'body': 'A reply to myself',
          });
          await _until(
            () =>
                active.calls
                    .where((call) => call.$1 == 'send_chat_message')
                    .length ==
                2,
            'reply to local echo',
          );
          final options = runtimeMap(
            active.calls
                .lastWhere((call) => call.$1 == 'send_chat_message')
                .$2['options'],
          );
          expect(runtimeMap(options['reply'])['author_jid'], 'me@example.org');
          active.responses['fetch_dm_history'] = {
            'messages': [
              _archive(id, 'My archived direct reply', 'second-own-archive'),
            ],
            'is_complete': true,
          };
          await runtime.conversations.invoke('conversation.history', view);
          await _flush();
          expect(
            rows().where((row) => row['body'] == 'My archived direct reply'),
            hasLength(3),
          );
          await expectLater(
            runtime.conversations.invoke('conversation.reply', {
              ...view,
              'targetId': id,
            }),
            throwsStateError,
          );
        }
      },
      skip: Platform.environment['WADDLE_FLUTTER_RUNTIME_LIBRARY'] == null
          ? 'Build the real Rust runtime and set WADDLE_FLUTTER_RUNTIME_LIBRARY.'
          : false,
    );
  }

  test(
    'room archive before live reflection keeps its own attachment on ACK',
    () async {
      const room = 'general@rooms.example.org';
      final storage = _Storage()..values['session'] = 'private-token';
      final active = _Transport(
        timeline: NativeRustTransport(
          FfiRustBindings.load(),
          pollInterval: const Duration(milliseconds: 1),
        ),
      );
      final runtime = SessionRuntime(
        storage: storage,
        auth: _Auth(),
        transportFactory: () async => active,
        platformInvoker: (_, _) async => null,
      );
      final events = <Map<String, dynamic>>[];
      final subscription = runtime.events.listen(events.add);
      addTearDown(subscription.cancel);
      addTearDown(runtime.dispose);
      await runtime.start();
      await _until(
        () => active.calls.any((call) => call.$1 == 'connect'),
        'connect',
      );
      active.controller.add({'type': 'connected', 'data': null});
      await _until(
        () => active.calls.any((call) => call.$1 == 'discover_topology'),
        'initialization',
      );
      final reply = runtime.conversations.notificationReply(
        jid: room,
        group: true,
        body: 'My room send',
        waitForConnection: true,
      );
      await _until(
        () => active.calls.any((call) => call.$1 == 'send_groupchat_message'),
        'room send',
      );
      final id =
          runtimeRows(
                runtime.persistent['conversationOutbox'],
              ).single['origin_id']
              as String;
      await runtime.conversations.onEvent({
        'type': 'presence',
        'data': {
          'from': '$room/me',
          'muc_jid': 'me@example.org',
          'muc_status_codes': [110],
          'muc_affiliation': 'member',
        },
      });
      Map<String, dynamic> file(String name) => {
        'url': 'https://files.example.org/$name',
        'name': name,
        'media_type': 'image/png',
        'size': 3,
        'hashes': <dynamic>[],
        'encrypted': null,
      };
      final archived = {
        'id': id,
        'origin_id': id,
        'from': '$room/me',
        'message_type': 'groupchat',
        'is_muc': true,
        'author_real_jid': 'me@example.org',
        'mam_id': 'room-stamp',
        'body': 'Archive content',
        'timestamp': '2026-10-31T09:00:00Z',
        'shared_files': [file('archive.png')],
      };
      active.responses['fetch_room_history'] = {
        'messages': [archived],
        'is_complete': true,
      };
      active.responses['fetch_room_pins'] = <dynamic>[];
      const view = {
        'jid': room,
        'group': true,
        'viewId': 'room',
        'atNewestEdge': false,
      };
      await runtime.conversations.invoke('conversation.open', view);
      await _until(
        () => events.any(
          (event) =>
              event['topic'] == 'conversation' &&
              runtimeRows(
                runtimeMap(event['data'])['messages'],
              ).any((row) => row['body'] == 'Archive content'),
        ),
        'archive before reflection',
      );
      await runtime.conversations.onEvent({
        'type': 'message',
        'data': {
          ...archived,
          'mam_id': null,
          'stanza_id': 'room-stamp',
          'stanza_id_by': room,
          'body': 'My room send',
          'shared_files': [file('live.png')],
        },
      });
      await runtime.conversations.onEvent({
        'type': 'deliveryAcked',
        'data': {'stanza_id': id},
      });
      await reply.timeout(const Duration(seconds: 3));
      await _flush();
      final last = events.lastWhere(
        (event) => event['topic'] == 'conversation',
      );
      final rows = runtimeRows(runtimeMap(last['data'])['messages']);
      expect(rows.where((row) => row['pending'] == true), isEmpty);
      expect(rows, hasLength(1));
      expect(rows.single['id'], 'room-stamp');
      expect(runtimeRows(rows.single['files']).single['name'], 'archive.png');
      expect(runtimeRows(runtime.persistent['conversationOutbox']), isEmpty);
    },
    skip: Platform.environment['WADDLE_FLUTTER_RUNTIME_LIBRARY'] == null
        ? 'Build the real Rust runtime and set WADDLE_FLUTTER_RUNTIME_LIBRARY.'
        : false,
  );

  test(
    'live call and pending signaling retain a hidden push connection',
    () async {
      final storage = _Storage()..values['session'] = 'private-token';
      final transport = _Transport();
      final platform = StreamController<Map<String, dynamic>>.broadcast();
      final runtime = SessionRuntime(
        storage: storage,
        auth: _Auth(),
        initiallyVisible: false,
        transportFactory: () async => transport,
        platformInvoker: (_, _) async => null,
      );
      runtime.attachPlatformEvents(platform.stream);
      addTearDown(platform.close);
      addTearDown(runtime.dispose);
      await runtime.start();
      await _flush();
      platform.add({
        'topic': 'call',
        'generation': runtime.generation,
        'data': {'state': 'active'},
      });
      await _flush();
      await runtime.effectiveBackgroundDemand(false, pushEnabled: true);
      expect(transport.closed, false);
      platform.add({
        'topic': 'call',
        'generation': runtime.generation,
        'data': {'state': 'ended', 'signaling': true},
      });
      await _flush();
      expect(transport.closed, false);
      platform.add({
        'topic': 'call',
        'generation': runtime.generation,
        'data': {'state': 'ended', 'signaling': false},
      });
      await _flush();
      expect(transport.closed, true);
    },
  );

  test(
    'failed retirement fences replacements until the old close succeeds',
    () async {
      final storage = _Storage()..values['session'] = 'private-token';
      var failClose = true;
      final first = _Transport()
        ..closeHandler = () async {
          if (failClose) {
            throw StateError('socket is not quiescent');
          }
        };
      var factories = 0;
      final runtime = SessionRuntime(
        storage: storage,
        auth: _Auth(),
        transportFactory: () async => factories++ == 0 ? first : _Transport(),
        platformInvoker: (_, _) async => null,
      );
      await runtime.start();
      await _flush();
      await runtime.onVisibility(false);
      await expectLater(
        runtime.effectiveBackgroundDemand(false, pushEnabled: true),
        throwsStateError,
      );
      await expectLater(runtime.onVisibility(true), throwsStateError);
      expect(factories, 1);
      expect(first.closed, false);
      failClose = false;
      await runtime.invoke('session.retry', {});
      expect(first.closed, true);
      expect(factories, 2);
      await runtime.dispose();
    },
  );

  test(
    'failed disposal is never acknowledged and can retry the same old handle',
    () async {
      final storage = _Storage()..values['session'] = 'private-token';
      var failClose = true;
      final transport = _Transport()
        ..closeHandler = () async {
          if (failClose) {
            throw StateError('socket is not quiescent');
          }
        };
      final runtime = SessionRuntime(
        storage: storage,
        auth: _Auth(),
        transportFactory: () async => transport,
        platformInvoker: (_, _) async => null,
      );
      await runtime.start();
      await _flush();
      await expectLater(runtime.dispose(), throwsStateError);
      await expectLater(runtime.dispose(), throwsStateError);
      expect(transport.closeCalls, 2);
      failClose = false;
      await runtime.dispose();
      expect(transport.closed, true);
      expect(transport.closeCalls, 3);
    },
  );

  test(
    'handoff waits for a pending factory and its disconnect fence',
    () async {
      final storage = _Storage()..values['session'] = 'private-token';
      final factory = Completer<RustTransport>();
      final closed = Completer<void>();
      final transport = _Transport()..closeHandler = () => closed.future;
      final runtime = SessionRuntime(
        storage: storage,
        auth: _Auth(),
        transportFactory: () => factory.future,
        platformInvoker: (_, _) async => null,
      );
      await runtime.start();
      await _flush();
      var released = false;
      final disposed = runtime.dispose().then((_) => released = true);
      await _flush();
      expect(released, false);
      factory.complete(transport);
      await _flush();
      expect(released, false);
      expect(transport.calls.where((call) => call.$1 == 'connect'), isEmpty);
      closed.complete();
      await disposed;
      expect(released, true);
      expect(transport.closed, true);
    },
  );

  test(
    'native resync overflow replaces the quarantined transport immediately',
    () async {
      final storage = _Storage()..values['session'] = 'private-token';
      final handles = <_Transport>[];
      final runtime = SessionRuntime(
        storage: storage,
        auth: _Auth(),
        reconnectDelay: (_) => Duration.zero,
        transportFactory: () async {
          final handle = _Transport();
          handles.add(handle);
          return handle;
        },
        platformInvoker: (_, _) async => null,
      );
      addTearDown(runtime.dispose);
      await runtime.start();
      await _flush();
      handles.single.controller.add({
        'type': 'error',
        'data': {'code': 'resync_required', 'message': 'queue overflow'},
      });
      await _flush();
      expect(handles, hasLength(2));
      expect(handles.first.closed, true);
    },
  );

  test('logout stops call signaling before clearing its identity', () async {
    final storage = _Storage()..values['session'] = 'private-token';
    final transport = _Transport();
    final platform = StreamController<Map<String, dynamic>>.broadcast();
    final operations = <String>[];
    late final SessionRuntime runtime;
    runtime = SessionRuntime(
      storage: storage,
      auth: _Auth(),
      transportFactory: () async => transport,
      platformInvoker: (method, args) async {
        operations.add(method);
        expect(runtime.jid, 'me@example.org');
        expect(runtime.session['fullJid'], isNotNull);
        expect(args['generation'], runtime.generation);
      },
    );
    addTearDown(platform.close);
    addTearDown(runtime.dispose);
    runtime.attachPlatformEvents(platform.stream);
    await runtime.start();
    await _flush();
    platform.add({
      'topic': 'call',
      'generation': runtime.generation,
      'data': {'state': 'active'},
    });
    await _flush();
    await runtime.invoke('session.logout', {});
    expect(operations, ['call.hangup']);
    expect(runtime.session['state'], 'signedOut');
    expect(transport.closed, true);
    expect(storage.values['session'], isNull);
  });

  test(
    'logout clears saved credentials even if socket retirement fails',
    () async {
      final storage = _Storage()..values['session'] = 'private-token';
      final auth = _Auth();
      var failClose = true;
      final transport = _Transport()
        ..closeHandler = () async {
          if (failClose) {
            throw StateError('socket is not quiescent');
          }
        };
      final runtime = SessionRuntime(
        storage: storage,
        auth: auth,
        transportFactory: () async => transport,
        platformInvoker: (_, _) async => null,
      );
      await runtime.start();
      await _flush();
      await expectLater(runtime.invoke('session.logout', {}), throwsStateError);
      expect(runtime.session['state'], 'signedOut');
      expect(storage.values['session'], isNull);
      expect(auth.revoked, ['private-token']);
      expect(transport.closed, false);
      failClose = false;
      await runtime.dispose();
      expect(transport.closed, true);
    },
  );
  test(
    'runtime handoff flushes queued account data before disconnect acknowledgement',
    () async {
      final storage = _DelayedStorage()..values['session'] = 'private-token';
      final transport = _Transport();
      final runtime = SessionRuntime(
        storage: storage,
        auth: _Auth(),
        transportFactory: () async => transport,
        platformInvoker: (_, _) async => null,
      );
      await runtime.start();
      await _flush();
      storage.accountWrite = Completer<void>();
      runtime.persistent['conversationOutbox'] = [
        {'body': 'Retained draft'},
      ];
      final written = runtime.persist();
      await _flush();
      var released = false;
      final disposed = runtime.dispose().then((_) => released = true);
      await _flush();
      expect(released, false);
      expect(transport.closed, false);
      storage.accountWrite!.complete();
      await written;
      await disposed;
      expect(
        storage.values['account:me@example.org'],
        contains('Retained draft'),
      );
      expect(transport.closed, true);
      expect(released, true);
    },
  );

  test(
    'parallel room opens share one join and preserve its durable membership',
    () async {
      final storage = _Storage()..values['session'] = 'private-token';
      final joined = Completer<dynamic>();
      final transport = _Transport()
        ..handler = (method, args) async =>
            method == 'join_room' ? joined.future : null;
      final runtime = SessionRuntime(
        storage: storage,
        auth: _Auth(),
        transportFactory: () async => transport,
        platformInvoker: (_, _) async => null,
      );
      addTearDown(runtime.dispose);
      await runtime.start();
      await _flush();
      final first = runtime.rpc('join_room', {
        'room_jid': 'room@rooms.example.org',
        'nick': 'me',
      });
      final second = runtime.rpc('join_room', {
        'room_jid': 'room@rooms.example.org',
        'nick': 'me',
      });
      await _flush();
      expect(transport.calls.where((call) => call.$1 == 'join_room').length, 1);
      joined.complete(null);
      await Future.wait([first, second]);
      await runtime.rpc('join_room', {
        'room_jid': 'room@rooms.example.org',
        'nick': 'me',
      });
      expect(transport.calls.where((call) => call.$1 == 'join_room').length, 1);
      expect(runtime.persistent['joinedRooms'], ['room@rooms.example.org']);
    },
  );

  test(
    'failed REST validation keeps saved credentials and offers retry',
    () async {
      final storage = _Storage()..values['session'] = 'private-token';
      final auth = _Auth()
        ..readSession = (_) async => throw const AuthFailure(transient: true);
      var factories = 0;
      final runtime = SessionRuntime(
        storage: storage,
        auth: auth,
        transportFactory: () async {
          factories++;
          return _Transport();
        },
        platformInvoker: (_, _) async => null,
      );
      addTearDown(runtime.dispose);
      final events = <Map<String, dynamic>>[];
      final subscription = runtime.events.listen(events.add);
      addTearDown(subscription.cancel);
      await runtime.start();
      await _flush();
      expect(runtime.session['state'], 'error');
      expect(storage.values['session'], 'private-token');
      expect(factories, 0);
      expect(events.toString(), isNot(contains('private-token')));
    },
  );

  test('cancelled device flow ignores a late completed poll', () async {
    final poll = Completer<Map<String, dynamic>>();
    final auth = _Auth()..poll = (_) => poll.future;
    final storage = _Storage();
    var factories = 0;
    final runtime = SessionRuntime(
      storage: storage,
      auth: auth,
      transportFactory: () async {
        factories++;
        return _Transport();
      },
      platformInvoker: (_, _) async => null,
      delay: (_) async {},
    );
    addTearDown(runtime.dispose);
    await runtime.start();
    await runtime.invoke('login.select', {'id': 'community'});
    await _flush();
    await runtime.invoke('login.cancel', {});
    poll.complete({'status': 'complete', 'session_id': 'late-token'});
    await _flush();
    expect(auth.sessionReads, 0);
    expect(runtime.session['state'], 'signedOut');
    expect(storage.values['session'], isNull);
    expect(factories, 0);
  });

  test(
    'restored account connects with its private token and resumption disabled',
    () async {
      final storage = _Storage()..values['session'] = 'private-token';
      final transport = _Transport();
      final runtime = SessionRuntime(
        storage: storage,
        auth: _Auth(),
        transportFactory: () async => transport,
        platformInvoker: (_, _) async => null,
      );
      addTearDown(runtime.dispose);
      final events = <Map<String, dynamic>>[];
      final subscription = runtime.events.listen(events.add);
      addTearDown(subscription.cancel);
      await runtime.start();
      await _flush();
      final connect = transport.calls
          .singleWhere((call) => call.$1 == 'connect')
          .$2;
      expect(connect['access_token'], 'private-token');
      expect(connect['resumable'], false);
      expect(
        runtime.session['fullJid'],
        'me@example.org/${connect['resource']}',
      );
      expect(events.toString(), isNot(contains('private-token')));
      expect(events.toString(), isNot(contains('private-device-code')));
    },
  );

  test(
    'logout fences an unfinished profile read before its follow-up RPCs',
    () async {
      final storage = _Storage()..values['session'] = 'private-token';
      final card = Completer<dynamic>();
      final transport = _Transport()
        ..handler = (method, args) async =>
            method == 'fetch_vcard4' ? card.future : null;
      final runtime = SessionRuntime(
        storage: storage,
        auth: _Auth(),
        transportFactory: () async => transport,
        platformInvoker: (_, _) async => null,
      );
      addTearDown(runtime.dispose);
      await runtime.start();
      await _flush();
      final pending = runtime.invoke('profile.load', {});
      final assertion = expectLater(pending, throwsA(isA<AccountChanged>()));
      await _flush();
      await runtime.invoke('session.logout', {});
      card.complete({'full_name': 'Old account private name'});
      await assertion;
      expect(
        transport.calls.where((call) => call.$1 == 'fetch_user_pep_profile'),
        isEmpty,
      );
      expect(storage.values['session'], isNull);
      expect(runtime.session['state'], 'signedOut');
    },
  );

  test(
    'invalid persisted session signs out and retains user preferences',
    () async {
      final storage = _Storage()
        ..values.addAll({
          'session': 'expired',
          'preferences': '{"theme":"dark"}',
        });
      final auth = _Auth()..readSession = (_) async => null;
      final runtime = SessionRuntime(
        storage: storage,
        auth: auth,
        transportFactory: () async => _Transport(),
        platformInvoker: (_, _) async => null,
      );
      addTearDown(runtime.dispose);
      await runtime.start();
      expect(runtime.session['state'], 'signedOut');
      expect(runtime.preferences['theme'], 'dark');
      expect(storage.values['session'], isNull);
      expect(storage.values['preferences'], isNotNull);
    },
  );
}

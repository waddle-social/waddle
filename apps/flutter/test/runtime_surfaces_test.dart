import 'dart:async';

import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_flutter/runtime/context.dart';
import 'package:waddle_flutter/runtime/surfaces_runtime.dart';
import 'package:waddle_flutter/transport/transport.dart';

class _Context implements RuntimeContext {
  @override
  int generation = 1;
  @override
  String? jid = 'me@example.org';
  @override
  RustTransport? get transport => null;
  @override
  final preferences = <String, dynamic>{};
  @override
  final session = <String, dynamic>{'xmppLocalpart': 'me'};
  @override
  final persistent = <String, dynamic>{};
  final calls = <(String, Map<String, dynamic>)>[];
  final snapshots = <String, Map<String, dynamic>>{};
  final responses = <String, dynamic>{};
  Future<dynamic> Function(String, Map<String, dynamic>)? handler;
  int refreshes = 0;
  @override
  void emit(String topic, Map<String, dynamic> data) {
    snapshots[topic] = data;
  }

  @override
  Future<dynamic> rpc(
    String method, [
    Map<String, dynamic> args = const {},
  ]) async {
    calls.add((method, args));
    return handler == null ? responses[method] : handler!(method, args);
  }

  @override
  Future<dynamic> platform(
    String method, [
    Map<String, dynamic> args = const {},
  ]) async => throw UnsupportedError(method);
  @override
  Future<void> persist() async {}
  @override
  Future<void> refreshHome() async {
    refreshes++;
  }
}

void main() {
  test(
    'group creation includes self and joins returned room through Rust',
    () async {
      final context = _Context()
        ..responses['create_group_dm'] = 'group@rooms.example.org';
      final surface = SurfaceRuntime(context);
      addTearDown(surface.dispose);
      final result = await surface.invoke('group.create', {
        'name': 'Friends',
        'members': ['alex@example.org', 'alex@example.org'],
      });
      expect(context.calls[0].$1, 'create_group_dm');
      expect(context.calls[0].$2, {
        'name': 'Friends',
        'member_jids': ['me@example.org', 'alex@example.org'],
      });
      expect(context.calls[1].$1, 'join_room');
      expect(context.calls[1].$2, {
        'room_jid': 'group@rooms.example.org',
        'nick': 'me',
      });
      expect(result, {'jid': 'group@rooms.example.org'});
      expect(context.refreshes, 1);
    },
  );

  test(
    'profile publish failure restores the previously loaded profile',
    () async {
      final context = _Context()
        ..responses['fetch_vcard4'] = {
          'full_name': 'Original',
          'photo_uri': 'xmpp:avatar',
        };
      final surface = SurfaceRuntime(context);
      addTearDown(surface.dispose);
      await surface.invoke('profile.load', {});
      context.handler = (method, args) async {
        if (method == 'publish_vcard4') {
          throw StateError('forbidden');
        }
        return context.responses[method];
      };
      await expectLater(
        surface.invoke('profile.save', {'fullName': 'Changed'}),
        throwsStateError,
      );
      expect(context.snapshots['profile']?['fullName'], 'Original');
      final published = context.calls.last.$2['vcard'] as Map;
      expect(published['full_name'], 'Changed');
      expect(published['photo_uri'], 'xmpp:avatar');
    },
  );

  test(
    'account change during profile fetch prevents cross-account follow-up work',
    () async {
      final context = _Context();
      final fetch = Completer<dynamic>();
      context.handler = (method, _) async =>
          method == 'fetch_vcard4' ? fetch.future : null;
      final surface = SurfaceRuntime(context);
      addTearDown(surface.dispose);
      final pending = surface.invoke('profile.load', {});
      final assertion = expectLater(pending, throwsA(isA<AccountChanged>()));
      await Future<void>.delayed(Duration.zero);
      context.generation++;
      context.jid = 'successor@example.org';
      surface.onAccountChanged();
      fetch.complete({'full_name': 'Private old name'});
      await assertion;
      expect(context.calls.length, 1);
      expect(context.snapshots['profile'], isNull);
    },
  );

  test('room updates use native GET-merge-SET patch contract', () async {
    final context = _Context();
    final surface = SurfaceRuntime(context);
    addTearDown(surface.dispose);
    await surface.invoke('room.save', {
      'jid': 'general@rooms.example.org',
      'name': 'General',
      'description': 'Hello',
      'forum': true,
      'pinPermission': 'moderators',
    });
    expect(context.calls.single.$1, 'submit_room_config');
    expect(context.calls.single.$2, {
      'room_jid': 'general@rooms.example.org',
      'patch': {
        'name': 'General',
        'description': 'Hello',
        'forum': true,
        'pin_permission': 'moderators',
      },
    });
  });

  test(
    'false protocol outcomes and unknown UI commands never report success',
    () async {
      final context = _Context()..responses['retract_sticker_pack'] = false;
      final surface = SurfaceRuntime(context);
      addTearDown(surface.dispose);
      await expectLater(
        surface.invoke('stickers.remove', {'packId': 'pack'}),
        throwsStateError,
      );
      await expectLater(
        surface.invoke('invented.success', {}),
        throwsUnsupportedError,
      );
      expect(context.calls.length, 1);
    },
  );

  test(
    'extension submit rejects fields not returned by its own form',
    () async {
      final context = _Context()
        ..responses['invoke_extension_command'] = {
          'status': 'executing',
          'session_id': 'session',
          'form': {
            'fields': [
              {'name': 'allowed', 'type': 'text_single', 'values': []},
            ],
          },
        };
      final surface = SurfaceRuntime(context);
      addTearDown(surface.dispose);
      await surface.invoke('slash.invoke', {
        'serviceJid': 'extensions.example.org',
        'node': 'test',
      });
      await expectLater(
        surface.invoke('slash.submit', {
          'serviceJid': 'extensions.example.org',
          'node': 'test',
          'sessionId': 'session',
          'fields': [
            {
              'name': 'injected',
              'values': ['secret'],
            },
          ],
        }),
        throwsArgumentError,
      );
      expect(
        context.calls.where(
          (call) => call.$1 == 'submit_extension_command_form',
        ),
        isEmpty,
      );
    },
  );
}

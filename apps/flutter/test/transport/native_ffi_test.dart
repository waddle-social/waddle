import 'dart:ffi';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_flutter/transport/native_bindings.dart';
import 'package:waddle_flutter/transport/transport.dart';
import 'package:waddle_flutter/transport/transport_native.dart';

void main() {
  final library = Platform.environment['WADDLE_FLUTTER_RUNTIME_LIBRARY'];
  test(
    'real C ABI accepts nested encrypted/profile/group arguments and owns returned buffers',
    () async {
      final bindings = FfiRustBindings(DynamicLibrary.open(library!));
      final transport = NativeRustTransport(bindings);
      final options = {
        'stanza_id': 'ffi-fixture',
        'subject': null,
        'reply': null,
        'fallback': null,
        'thread': {'id': 'thread-1', 'parent': null},
        'markup_spans': [],
        'references': [],
        'shared_files': [
          {
            'url': 'https://files.example.test/cipher.bin',
            'name': 'image.png',
            'media_type': 'image/png',
            'size': 3,
            'width': 10,
            'height': 10,
            'hashes': [],
            'disposition': 'inline',
            'encrypted': {
              'cipher': 'urn:xmpp:ciphers:aes-256-gcm-nopadding:0',
              'key_b64': 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=',
              'iv_b64': 'AAAAAAAAAAAAAAAA',
              'hashes': [],
              'sources': ['https://files.example.test/cipher.bin'],
            },
          },
        ],
        'link_preview_token': null,
        'request_displayed_marker': true,
        'muc_pm': false,
        'sticker': null,
        'mention_directory': null,
      };
      final commands = <String, Map<String, dynamic>>{
        'send_chat_message': {
          'peer_jid': 'bob@example.test',
          'body': '',
          'options': options,
        },
        'publish_vcard4': {
          'vcard': {
            'full_name': 'Alice',
            'nickname': 'alice',
            'pronouns': 'she/her',
            'note': 'Hello',
            'url': 'https://example.test',
            'photo_uri': null,
          },
        },
        'publish_tune': {
          'tune': {
            'artist': 'Artist',
            'title': 'Track',
            'source': 'Album',
            'track': '1',
            'uri': null,
            'length_seconds': 180,
            'rating': 8,
          },
        },
        'create_group_dm': {
          'name': 'Friends',
          'member_jids': ['alice@example.test', 'bob@example.test'],
        },
        'submit_room_config': {
          'room_jid': 'room@muc.example.test',
          'patch': {
            'name': 'Room',
            'description': 'Description',
            'forum': false,
            'pin_permission': 'anyone',
          },
        },
        'register_push_device': {
          'push_service_jid': 'push.example.test',
          'app_id': 'social.waddle.flutter',
          'environment': 'production',
          'credentials': {
            'type': 'web_push',
            'endpoint': 'https://push.example.test/endpoint',
            'p256dh': 'key',
            'auth': 'auth',
          },
        },
      };
      try {
        for (final entry in commands.entries) {
          await expectLater(
            transport.rpc(entry.key, entry.value),
            throwsA(
              isA<RustTransportException>().having(
                (e) => e.code,
                '${entry.key} parsed before connection',
                'not_connected',
              ),
            ),
          );
        }
        final result = await transport.rpc('timeline_snapshot', {
          'conversation_jid': 'bob@example.test',
        });
        expect(result, isA<Map>());
      } finally {
        await transport.close();
      }
      // Repeated lifetimes exercise native ownership and free_json, not a mock allocator.
      for (var i = 0; i < 8; i++) {
        final handle = bindings.create();
        expect(handle, greaterThan(0));
        expect(bindings.poll(handle), '[]');
        bindings.destroy(handle);
      }
    },
    skip: library == null
        ? 'Set WADDLE_FLUTTER_RUNTIME_LIBRARY to the built host cdylib.'
        : false,
  );
}

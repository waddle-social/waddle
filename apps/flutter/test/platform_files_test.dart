import 'dart:async';
import 'dart:convert';
import 'dart:typed_data';
import 'package:cryptography/cryptography.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:waddle_flutter/runtime/context.dart';
import 'package:waddle_flutter/runtime/platform_files.dart';
import 'platform_test_context.dart';

Future<({Uint8List ciphertext, Map<String, dynamic> envelope})> encrypted(
  List<int> plain,
) async {
  final key = SecretKey(List.generate(32, (i) => i));
  final nonce = List.generate(12, (i) => i + 1);
  final box = await AesGcm.with256bits().encrypt(
    plain,
    secretKey: key,
    nonce: nonce,
  );
  final ciphertext = Uint8List.fromList([...box.cipherText, ...box.mac.bytes]);
  return (
    ciphertext: ciphertext,
    envelope: {
      'cipher': aes256GcmCipher,
      'key_b64': base64Encode(await key.extractBytes()),
      'iv_b64': base64Encode(nonce),
      'hashes': [
        {
          'algo': 'sha-256',
          'value_b64': base64Encode((await Sha256().hash(ciphertext)).bytes),
        },
      ],
    },
  );
}

void main() {
  test(
    'decrypts GCM ciphertext only after validating advertised hashes and tag',
    () async {
      final plain = utf8.encode('private attachment');
      final fixture = await encrypted(plain);
      expect(
        await decryptAttachment(fixture.ciphertext, fixture.envelope),
        plain,
      );
      final corrupt = Uint8List.fromList(fixture.ciphertext)..[0] ^= 1;
      await expectLater(
        decryptAttachment(corrupt, fixture.envelope),
        throwsStateError,
      );
      final badTag = Uint8List.fromList(fixture.ciphertext)
        ..[fixture.ciphertext.length - 1] ^= 1;
      await expectLater(
        decryptAttachment(badTag, {...fixture.envelope, 'hashes': []}),
        throwsA(isA<SecretBoxAuthenticationError>()),
      );
      await expectLater(
        decryptAttachment(fixture.ciphertext, {
          ...fixture.envelope,
          'cipher': 'urn:unknown',
        }),
        throwsStateError,
      );
    },
  );

  test(
    'uploads encrypted bytes to XMPP slot and returns a decryptable shared-file envelope',
    () async {
      final context = TestRuntimeContext();
      http.Request? put;
      context.handler = (method, args) async =>
          method == 'discover_upload_service'
          ? 'upload.example.test'
          : {
              'put_url': 'https://files.example.test/put',
              'get_url': 'https://files.example.test/get',
              'put_headers': [],
            };
      final service = PlatformFileService(
        context,
        client: MockClient((request) async {
          put = request;
          return http.Response('', 201);
        }),
      );
      final plain = Uint8List.fromList(utf8.encode('secret photograph'));
      final file = await service.upload({
        'bytes': plain,
        'name': 'photo.png',
        'media_type': 'image/png',
      });
      expect(put!.bodyBytes, isNot(plain));
      expect(put!.headers['Content-Type'], 'application/octet-stream');
      expect(
        await decryptAttachment(put!.bodyBytes, runtimeMap(file['encrypted'])),
        plain,
      );
      expect(file['name'], 'photo.png');
      expect(file['size'], plain.length);
      expect(context.calls.last['method'], 'request_upload_slot');
      expect(
        (context.calls.last['args'] as Map)['filename'],
        isNot('photo.png'),
      );
      await service.dispose();
    },
  );

  test(
    'rejects HTTP slots and fences an account change before upload',
    () async {
      final context = TestRuntimeContext();
      var puts = 0;
      final slot = Completer<dynamic>();
      final slotRequested = Completer<void>();
      context.handler = (method, args) async {
        if (method == 'discover_upload_service') return 'upload.example.test';
        slotRequested.complete();
        return await slot.future;
      };
      final service = PlatformFileService(
        context,
        client: MockClient((_) async {
          puts++;
          return http.Response('', 201);
        }),
      );
      final pending = service.upload({
        'bytes': Uint8List.fromList([1, 2, 3]),
        'name': 'x',
      });
      final failure = expectLater(pending, throwsA(isA<AccountChanged>()));
      await slotRequested.future;
      context.generation++;
      slot.complete({
        'put_url': 'https://files.example.test/put',
        'get_url': 'https://files.example.test/get',
        'put_headers': [],
      });
      await failure;
      expect(puts, 0);
      context.handler = (method, args) async =>
          method == 'discover_upload_service'
          ? 'upload.example.test'
          : {
              'put_url': 'http://files.example.test/put',
              'get_url': 'https://files.example.test/get',
              'put_headers': [],
            };
      await expectLater(
        service.upload({
          'bytes': Uint8List.fromList([1]),
          'name': 'x',
        }),
        throwsStateError,
      );
      expect(puts, 0);
      await service.dispose();
    },
  );

  test(
    'tries verified ciphertext sources in order without exposing plaintext to network',
    () async {
      final fixture = await encrypted([4, 5, 6]);
      final requests = <Uri>[];
      final service = PlatformFileService(
        TestRuntimeContext(),
        client: MockClient((request) async {
          requests.add(request.url);
          return request.url.path == '/bad'
              ? http.Response('', 503)
              : http.Response.bytes(fixture.ciphertext, 200);
        }),
      );
      final file = {
        'url': 'https://files.example.test/bad',
        'size': 3,
        'encrypted': {
          ...fixture.envelope,
          'sources': [
            'https://files.example.test/bad',
            'https://files.example.test/good',
          ],
        },
      };
      expect(await service.bytes(file), [4, 5, 6]);
      expect(requests.map((url) => url.path), ['/bad', '/good']);
      await service.dispose();
    },
  );
}

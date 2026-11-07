import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';
import 'package:android_file_picker/android_file_picker.dart';
import 'package:cryptography/cryptography.dart';
import 'package:file_picker/file_picker.dart';
import 'package:file_selector/file_selector.dart' show XFile;
import 'package:flutter/foundation.dart'
    show TargetPlatform, debugDefaultTargetPlatformOverride;
import 'package:flutter/services.dart' show MethodChannel, PlatformException;
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:waddle_flutter/runtime/context.dart';
import 'package:waddle_flutter/runtime/platform_files.dart';
import 'platform_test_context.dart';

class _StreamingClient extends http.BaseClient {
  _StreamingClient(this.handler);
  final Future<http.StreamedResponse> Function(http.BaseRequest) handler;
  @override
  Future<http.StreamedResponse> send(http.BaseRequest request) =>
      handler(request);
}

final class _PickedFile extends PlatformFile {
  _PickedFile(this.name, this.data, {String? mediaType})
    : xFile = XFile.fromData(data, name: name, mimeType: mediaType);
  @override
  final String name;
  final Uint8List data;
  @override
  final XFile xFile;
  @override
  Uri get uri => Uri.parse('file:///picked/$name');
  @override
  int? lengthSync() => data.length;
  @override
  Future<int?> length() async => data.length;
  @override
  Future<Uint8List> readAsBytes() async => data;
  @override
  Stream<Uint8List> readAsByteStream() => Stream.value(data);
}

class _Picker extends FilePickerPlatform {
  _Picker(this.file);
  PlatformFile file;
  @override
  Future<PlatformFile?> pickFile({
    String? dialogTitle,
    String? initialDirectory,
    FileType type = FileType.any,
    List<String>? allowedExtensions,
    Function(FilePickerStatus)? onFileLoading,
    int compressionQuality = 0,
    AndroidOptions androidOptions = const AndroidOptions(),
    DarwinOptions darwinOptions = const DarwinOptions(),
    WindowsOptions windowsOptions = const WindowsOptions(),
    LinuxOptions linuxOptions = const LinuxOptions(),
    WebOptions webOptions = const WebOptions(),
  }) async => file;
}

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
  TestWidgetsFlutterBinding.ensureInitialized();
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

  test('cancels a stalled response body and retries the next source', () async {
    final fixture = await encrypted([4, 5, 6]);
    final canceled = Completer<void>();
    final aborted = Completer<void>();
    final requests = <String>[];
    final service = PlatformFileService(
      TestRuntimeContext(),
      downloadTimeout: const Duration(milliseconds: 30),
      client: _StreamingClient((request) async {
        requests.add(request.url.path);
        if (request.url.path == '/stalled') {
          final abortable = request as http.Abortable;
          unawaited(abortable.abortTrigger!.then((_) => aborted.complete()));
          final body = StreamController<List<int>>(
            onCancel: () => canceled.complete(),
          );
          body.add([1, 2]);
          return http.StreamedResponse(body.stream, 200);
        }
        return http.StreamedResponse(Stream.value(fixture.ciphertext), 200);
      }),
    );
    final result = await service.bytes({
      'size': 3,
      'encrypted': {
        ...fixture.envelope,
        'sources': [
          'https://files.example.test/stalled',
          'https://files.example.test/good',
        ],
      },
    });
    expect(result, [4, 5, 6]);
    await canceled.future.timeout(const Duration(seconds: 1));
    await aborted.future.timeout(const Duration(seconds: 1));
    expect(requests, ['/stalled', '/good']);
    await service.dispose();
  });

  test('a failed download is removed so the same file can retry', () async {
    var requests = 0;
    var canceled = false;
    final service = PlatformFileService(
      TestRuntimeContext(),
      downloadTimeout: const Duration(milliseconds: 30),
      client: _StreamingClient((_) async {
        requests++;
        if (requests == 1) {
          final body = StreamController<List<int>>(
            onCancel: () => canceled = true,
          );
          return http.StreamedResponse(body.stream, 200);
        }
        return http.StreamedResponse(Stream.value([7, 8]), 200);
      }),
    );
    final file = {'url': 'https://files.example.test/file'};
    await expectLater(service.bytes(file), throwsStateError);
    expect(canceled, isTrue);
    expect(await service.bytes(file), [7, 8]);
    expect(requests, 2);
    await service.dispose();
  });

  test(
    'a request that stalls before headers is aborted and can retry',
    () async {
      var requests = 0;
      final aborted = Completer<void>();
      final service = PlatformFileService(
        TestRuntimeContext(),
        downloadTimeout: const Duration(milliseconds: 30),
        client: _StreamingClient((request) async {
          requests++;
          if (requests == 1) {
            final abortable = request as http.Abortable;
            unawaited(abortable.abortTrigger!.then((_) => aborted.complete()));
            return Completer<http.StreamedResponse>().future;
          }
          return http.StreamedResponse(Stream.value([4]), 200);
        }),
      );
      final file = {'url': 'https://files.example.test/file'};
      await expectLater(service.bytes(file), throwsStateError);
      await aborted.future.timeout(const Duration(seconds: 1));
      expect(await service.bytes(file), [4]);
      expect(requests, 2);
      await service.dispose();
    },
  );

  test('an error response body is canceled before trying a fallback', () async {
    final canceled = Completer<void>();
    var requests = 0;
    final fixture = await encrypted([6]);
    final service = PlatformFileService(
      TestRuntimeContext(),
      downloadTimeout: const Duration(seconds: 1),
      client: _StreamingClient((_) async {
        requests++;
        if (requests == 1) {
          final body = StreamController<List<int>>(
            onCancel: () => canceled.complete(),
          );
          return http.StreamedResponse(body.stream, 503);
        }
        return http.StreamedResponse(Stream.value(fixture.ciphertext), 200);
      }),
    );
    expect(
      await service.bytes({
        'size': 1,
        'encrypted': {
          ...fixture.envelope,
          'sources': [
            'https://files.example.test/a',
            'https://files.example.test/b',
          ],
        },
      }),
      [6],
    );
    await canceled.future.timeout(const Duration(seconds: 1));
    expect(requests, 2);
    await service.dispose();
  });

  test(
    'account switch cancels an old body without canceling a new download',
    () async {
      final context = TestRuntimeContext();
      final started = Completer<void>();
      final canceled = Completer<void>();
      var requests = 0;
      final service = PlatformFileService(
        context,
        downloadTimeout: const Duration(seconds: 1),
        client: _StreamingClient((_) async {
          requests++;
          if (requests == 1) {
            final body = StreamController<List<int>>(
              onListen: () => started.complete(),
              onCancel: () => canceled.complete(),
            );
            return http.StreamedResponse(body.stream, 200);
          }
          return http.StreamedResponse(Stream.value([9]), 200);
        }),
      );
      final file = {'url': 'https://files.example.test/file'};
      final old = service.bytes(file);
      final oldFailure = expectLater(old, throwsA(isA<AccountChanged>()));
      await started.future;
      context.generation++;
      final cleanup = service.onAccountChanged();
      expect(await service.bytes(file), [9]);
      await oldFailure;
      await canceled.future.timeout(const Duration(seconds: 1));
      await cleanup;
      await service.dispose();
    },
  );

  test(
    'corrupt ciphertext source is rejected before the verified fallback',
    () async {
      final fixture = await encrypted([1, 2, 3]);
      final corrupt = Uint8List.fromList(fixture.ciphertext)..[0] ^= 1;
      var requests = 0;
      final service = PlatformFileService(
        TestRuntimeContext(),
        client: MockClient((_) async {
          requests++;
          return http.Response.bytes(
            requests == 1 ? corrupt : fixture.ciphertext,
            200,
          );
        }),
      );
      expect(
        await service.bytes({
          'size': 3,
          'encrypted': {
            ...fixture.envelope,
            'sources': [
              'https://files.example.test/a',
              'https://files.example.test/b',
            ],
          },
        }),
        [1, 2, 3],
      );
      expect(requests, 2);
      await service.dispose();
    },
  );

  test(
    'picker respects provider MIME and detects HEIC by extension or bytes',
    () async {
      final previousPicker = FilePickerPlatform.instance;
      final picker = _Picker(
        _PickedFile(
          'wrong.bin',
          Uint8List.fromList([1, 2]),
          mediaType: 'application/pdf',
        ),
      );
      FilePickerPlatform.instance = picker;
      addTearDown(() => FilePickerPlatform.instance = previousPicker);
      final context = TestRuntimeContext();
      context.handler = (method, args) async =>
          method == 'discover_upload_service'
          ? 'upload.example.test'
          : {
              'put_url': 'https://files.example.test/put',
              'get_url': 'https://files.example.test/get',
            };
      final service = PlatformFileService(
        context,
        client: MockClient((_) async => http.Response('', 201)),
      );
      addTearDown(service.dispose);
      final provided = await service.invoke('file.pickAndUpload', {}) as Map;
      expect(provided['media_type'], 'application/pdf');
      picker.file = _PickedFile('photo.HEIC', Uint8List.fromList([1, 2]));
      final extension = await service.invoke('file.pickAndUpload', {}) as Map;
      expect(extension['media_type'], 'image/heic');
      picker.file = _PickedFile(
        'photo',
        Uint8List.fromList([
          0,
          0,
          0,
          20,
          ...ascii.encode('ftypheic'),
          0,
          0,
          0,
          0,
          ...ascii.encode('mif1'),
        ]),
      );
      final signature = await service.invoke('file.pickAndUpload', {}) as Map;
      expect(signature['media_type'], 'image/heic');
    },
  );

  test(
    'Android picker resolves transient provider MIME before upload',
    () async {
      final directory = await Directory.systemTemp.createTemp('waddle-picker-');
      addTearDown(() => directory.delete(recursive: true));
      final cached = File('${directory.path}/document');
      await cached.writeAsBytes([1, 2, 3]);
      const originalUri = 'content://example.documents/document/private-id';
      const pickerChannel = MethodChannel(
        'miguelruivo.flutter.plugins.filepicker',
      );
      const metadataChannel = MethodChannel(
        'social.waddle.flutter/platform_files',
      );
      final messenger =
          TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
      final previousPicker = FilePickerPlatform.instance;
      FilePickerPlatform.instance = FilePickerAndroid();
      addTearDown(() {
        FilePickerPlatform.instance = previousPicker;
        messenger.setMockMethodCallHandler(pickerChannel, null);
        messenger.setMockMethodCallHandler(metadataChannel, null);
      });
      messenger.setMockMethodCallHandler(pickerChannel, (call) async {
        expect(call.method, 'any');
        expect((call.arguments as Map)['androidSafOptions'], {
          'grant': 'transient',
          'access': 'readOnly',
          'autoPersist': false,
        });
        return [
          {
            'path': cached.path,
            'name': 'document',
            'size': 3,
            'safHandle': {'uri': originalUri, 'access': 'readOnly'},
          },
        ];
      });
      String? type = 'APPLICATION/VND.OASIS.OPENDOCUMENT.SPREADSHEET';
      var shouldFail = false;
      var cleanups = 0;
      var cleanupFails = false;
      messenger.setMockMethodCallHandler(metadataChannel, (call) async {
        if (call.method == 'removePickedCopies') {
          expect(call.arguments, [cached.path]);
          cleanups++;
          return !cleanupFails;
        }
        expect(call.method, 'mimeTypes');
        expect(call.arguments, [originalUri]);
        if (shouldFail) throw PlatformException(code: 'revoked');
        return [type];
      });
      final context = TestRuntimeContext();
      context.handler = (method, args) async =>
          method == 'discover_upload_service'
          ? 'upload.example.test'
          : {
              'put_url': 'https://files.example.test/put',
              'get_url': 'https://files.example.test/get',
            };
      final service = PlatformFileService(
        context,
        client: MockClient((_) async => http.Response('', 201)),
      );
      addTearDown(service.dispose);

      final provided = await service.invoke('file.pickAndUpload', {}) as Map;
      expect(
        provided['media_type'],
        'application/vnd.oasis.opendocument.spreadsheet',
      );
      expect(
        jsonEncode([provided, context.calls]),
        isNot(contains(originalUri)),
      );

      type = 'image/png\r\nInjected: value';
      final malformed = await service.invoke('file.pickAndUpload', {}) as Map;
      expect(malformed['media_type'], 'application/octet-stream');
      shouldFail = true;
      final revoked = await service.invoke('file.pickAndUpload', {}) as Map;
      expect(revoked['media_type'], 'application/octet-stream');
      shouldFail = false;
      type = null;
      final unspecified = await service.invoke('file.pickAndUpload', {}) as Map;
      expect(unspecified['media_type'], 'application/octet-stream');
      type = 'application/octet-stream';
      await cached.writeAsBytes([
        0x89,
        0x50,
        0x4e,
        0x47,
        0x0d,
        0x0a,
        0x1a,
        0x0a,
      ]);
      final sniffed = await service.invoke('file.pickAndUpload', {}) as Map;
      expect(sniffed['media_type'], 'image/png');
      expect(cleanups, 5);
      cleanupFails = true;
      final before = context.calls.length;
      await expectLater(
        service.invoke('file.pickAndUpload', {}),
        throwsStateError,
      );
      expect(context.calls.length, before);
    },
  );

  test(
    'Android multiple picker resolves all types before reading files',
    () async {
      final directory = await Directory.systemTemp.createTemp('waddle-picker-');
      addTearDown(() => directory.delete(recursive: true));
      final first = File('${directory.path}/first');
      final second = File('${directory.path}/second');
      await first.writeAsBytes([]);
      await second.writeAsBytes([]);
      const pickerChannel = MethodChannel(
        'miguelruivo.flutter.plugins.filepicker',
      );
      const metadataChannel = MethodChannel(
        'social.waddle.flutter/platform_files',
      );
      final messenger =
          TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
      final previousPicker = FilePickerPlatform.instance;
      FilePickerPlatform.instance = FilePickerAndroid();
      addTearDown(() {
        FilePickerPlatform.instance = previousPicker;
        messenger.setMockMethodCallHandler(pickerChannel, null);
        messenger.setMockMethodCallHandler(metadataChannel, null);
      });
      const firstUri = 'content://example.documents/document/first';
      const secondUri = 'content://example.documents/document/second';
      messenger.setMockMethodCallHandler(pickerChannel, (call) async {
        expect(call.method, 'image');
        expect((call.arguments as Map)['androidSafOptions'], {
          'grant': 'transient',
          'access': 'readOnly',
          'autoPersist': false,
        });
        return [
          {
            'path': first.path,
            'name': 'first',
            'size': 1,
            'safHandle': {'uri': firstUri},
          },
          {
            'path': second.path,
            'name': 'second',
            'size': 1,
            'safHandle': {'uri': secondUri},
          },
        ];
      });
      var lookups = 0;
      messenger.setMockMethodCallHandler(metadataChannel, (call) async {
        if (call.method == 'removePickedCopies') {
          expect(call.arguments, [first.path, second.path]);
          return true;
        }
        lookups++;
        expect(call.arguments, [firstUri, secondUri]);
        await first.writeAsBytes([1]);
        await second.writeAsBytes([2]);
        return ['image/tiff', 'image/heic'];
      });
      final service = PlatformFileService(TestRuntimeContext());
      addTearDown(service.dispose);
      final images = await service.invoke('file.pickImages', {}) as List;
      expect(lookups, 1);
      expect(images.map((file) => file['media_type']), [
        'image/tiff',
        'image/heic',
      ]);
      expect(images.map((file) => file['bytes'].toList()), [
        [1],
        [2],
      ]);
    },
  );

  test(
    'picker cleanup completes before another service can start selecting',
    () async {
      final directory = await Directory.systemTemp.createTemp('waddle-picker-');
      addTearDown(() => directory.delete(recursive: true));
      final cached = File('${directory.path}/image');
      await cached.writeAsBytes([1]);
      const pickerChannel = MethodChannel(
        'miguelruivo.flutter.plugins.filepicker',
      );
      const fileChannel = MethodChannel('social.waddle.flutter/platform_files');
      final messenger =
          TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
      final previousPicker = FilePickerPlatform.instance;
      FilePickerPlatform.instance = FilePickerAndroid();
      addTearDown(() {
        FilePickerPlatform.instance = previousPicker;
        messenger.setMockMethodCallHandler(pickerChannel, null);
        messenger.setMockMethodCallHandler(fileChannel, null);
      });
      var picks = 0;
      messenger.setMockMethodCallHandler(pickerChannel, (_) async {
        picks++;
        return [
          {
            'path': cached.path,
            'name': 'image',
            'size': 1,
            'safHandle': {'uri': 'content://example.documents/image'},
          },
        ];
      });
      final cleanupStarted = Completer<void>();
      final finishCleanup = Completer<void>();
      addTearDown(() {
        if (!finishCleanup.isCompleted) finishCleanup.complete();
      });
      var cleanups = 0;
      messenger.setMockMethodCallHandler(fileChannel, (call) async {
        if (call.method == 'mimeTypes') return ['image/png'];
        expect(call.method, 'removePickedCopies');
        cleanups++;
        if (cleanups == 1) {
          cleanupStarted.complete();
          await finishCleanup.future;
        }
        return true;
      });
      final firstContext = TestRuntimeContext();
      final firstService = PlatformFileService(firstContext);
      final secondService = PlatformFileService(TestRuntimeContext());
      addTearDown(firstService.dispose);
      addTearDown(secondService.dispose);
      final first = expectLater(
        firstService.invoke('file.pickImages', {}),
        throwsA(isA<AccountChanged>()),
      );
      await cleanupStarted.future;
      final second = secondService.invoke('file.pickImages', {});
      await Future<void>.delayed(Duration.zero);
      expect(picks, 1);
      firstContext.generation++;
      finishCleanup.complete();
      await first;
      expect((await second as List).length, 1);
      expect(picks, 2);
      expect(cleanups, 2);
    },
  );

  test('Android MIME lookup is fenced when the account changes', () async {
    final directory = await Directory.systemTemp.createTemp('waddle-picker-');
    addTearDown(() => directory.delete(recursive: true));
    final cached = File('${directory.path}/document');
    await cached.writeAsBytes([1]);
    const pickerChannel = MethodChannel(
      'miguelruivo.flutter.plugins.filepicker',
    );
    const metadataChannel = MethodChannel(
      'social.waddle.flutter/platform_files',
    );
    final messenger =
        TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
    final previousPicker = FilePickerPlatform.instance;
    FilePickerPlatform.instance = FilePickerAndroid();
    addTearDown(() {
      FilePickerPlatform.instance = previousPicker;
      messenger.setMockMethodCallHandler(pickerChannel, null);
      messenger.setMockMethodCallHandler(metadataChannel, null);
    });
    messenger.setMockMethodCallHandler(
      pickerChannel,
      (_) async => [
        {
          'path': cached.path,
          'name': 'document',
          'size': 1,
          'safHandle': {
            'uri': 'content://example.documents/document/private-id',
            'access': 'readOnly',
          },
        },
      ],
    );
    final lookupStarted = Completer<void>();
    final lookup = Completer<List<String?>>();
    var cleaned = false;
    messenger.setMockMethodCallHandler(metadataChannel, (call) async {
      if (call.method == 'removePickedCopies') {
        expect(call.arguments, [cached.path]);
        cleaned = true;
        return true;
      }
      lookupStarted.complete();
      return await lookup.future;
    });
    final context = TestRuntimeContext();
    final service = PlatformFileService(context);
    addTearDown(service.dispose);
    final picked = service.invoke('file.pickAndUpload', {});
    await lookupStarted.future;
    context.generation++;
    lookup.complete(['application/pdf']);
    await expectLater(picked, throwsA(isA<AccountChanged>()));
    expect(context.calls, isEmpty);
    expect(cleaned, isTrue);
  });

  test('clipboard image MIME and file name follow its encoded bytes', () async {
    debugDefaultTargetPlatformOverride = TargetPlatform.linux;
    addTearDown(() => debugDefaultTargetPlatformOverride = null);
    const channel = MethodChannel('pasteboard');
    TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .setMockMethodCallHandler(
          channel,
          (call) async => call.method == 'image'
              ? Uint8List.fromList([0xff, 0xd8, 0xff, 0x10])
              : null,
        );
    addTearDown(
      () => TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
          .setMockMethodCallHandler(channel, null),
    );
    final context = TestRuntimeContext();
    context.handler = (method, args) async =>
        method == 'discover_upload_service'
        ? 'upload.example.test'
        : {
            'put_url': 'https://files.example.test/put',
            'get_url': 'https://files.example.test/get',
          };
    final service = PlatformFileService(
      context,
      client: MockClient((_) async => http.Response('', 201)),
    );
    final file = await service.invoke('file.pasteAndUpload', {}) as Map;
    expect(file['media_type'], 'image/jpeg');
    expect(file['name'], 'image.jpg');
    await service.dispose();
  });

  test(
    'Android clipboard uses the bounded native reader and provider MIME',
    () async {
      debugDefaultTargetPlatformOverride = TargetPlatform.android;
      addTearDown(() => debugDefaultTargetPlatformOverride = null);
      const channel = MethodChannel('social.waddle.flutter/platform_files');
      const pasteboard = MethodChannel('pasteboard');
      final messenger =
          TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
      addTearDown(() {
        messenger.setMockMethodCallHandler(channel, null);
        messenger.setMockMethodCallHandler(pasteboard, null);
      });
      messenger.setMockMethodCallHandler(pasteboard, (_) async {
        fail('Android must not use the unbounded clipboard reader');
      });
      var result = <String, Object?>{
        'bytes': Uint8List.fromList([1, 2]),
        'mimeType': 'image/heic',
      };
      var oversized = false;
      var empty = false;
      messenger.setMockMethodCallHandler(channel, (call) async {
        expect(call.method, 'clipboardImage');
        expect(call.arguments, isNull);
        if (oversized) throw PlatformException(code: 'file_too_large');
        return empty ? null : result;
      });
      final context = TestRuntimeContext();
      context.handler = (method, args) async =>
          method == 'discover_upload_service'
          ? 'upload.example.test'
          : {
              'put_url': 'https://files.example.test/put',
              'get_url': 'https://files.example.test/get',
            };
      final service = PlatformFileService(
        context,
        client: MockClient((_) async => http.Response('', 201)),
      );
      addTearDown(service.dispose);
      final image = await service.invoke('file.pasteAndUpload', {}) as Map;
      expect(image['media_type'], 'image/heic');
      expect(image['name'], 'image.heic');
      empty = true;
      expect(await service.invoke('file.pasteAndUpload', {}), isNull);
      empty = false;
      oversized = true;
      await expectLater(
        service.invoke('file.pasteAndUpload', {}),
        throwsStateError,
      );
    },
  );

  test('Android clipboard lookup is fenced when the account changes', () async {
    debugDefaultTargetPlatformOverride = TargetPlatform.android;
    addTearDown(() => debugDefaultTargetPlatformOverride = null);
    const channel = MethodChannel('social.waddle.flutter/platform_files');
    final messenger =
        TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
    addTearDown(() => messenger.setMockMethodCallHandler(channel, null));
    final started = Completer<void>();
    final response = Completer<Map<String, Object?>>();
    messenger.setMockMethodCallHandler(channel, (_) {
      started.complete();
      return response.future;
    });
    final context = TestRuntimeContext();
    final service = PlatformFileService(context);
    addTearDown(service.dispose);
    final pasted = service.invoke('file.pasteAndUpload', {});
    await started.future;
    context.generation++;
    response.complete({
      'bytes': Uint8List.fromList([1]),
      'mimeType': 'image/png',
    });
    await expectLater(pasted, throwsA(isA<AccountChanged>()));
    expect(context.calls, isEmpty);
  });

  test(
    'limits the transmitted bytes including the GCM tag before discovery',
    () async {
      final context = TestRuntimeContext();
      final service = PlatformFileService(
        context,
        client: MockClient((_) async => http.Response('', 201)),
      );
      await expectLater(
        service.upload({'bytes': Uint8List(maxUploadBytes - 15)}),
        throwsStateError,
      );
      await expectLater(
        service.upload({
          'bytes': Uint8List(maxUploadBytes + 1),
        }, encrypt: false),
        throwsStateError,
      );
      expect(context.calls, isEmpty);
      context.handler = (method, args) async =>
          method == 'discover_upload_service'
          ? 'upload.example.test'
          : {
              'put_url': 'https://files.example.test/put',
              'get_url': 'https://files.example.test/get',
            };
      await service.upload({'bytes': Uint8List(maxUploadBytes - 16)});
      expect((context.calls.last['args'] as Map)['size'], maxUploadBytes);
      await service.dispose();
    },
  );
}

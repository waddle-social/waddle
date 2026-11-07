import 'dart:async';
import 'dart:convert';
import 'dart:math';
import 'dart:typed_data';
import 'dart:ui' as ui;

import 'package:cryptography/cryptography.dart';
import 'package:file_picker/file_picker.dart';
import 'package:http/http.dart' as http;
import 'package:image/image.dart' as img;

import 'context.dart';
import 'platform_file_access.dart';
import 'platform_file_channel.dart';

const aes256GcmCipher = 'urn:xmpp:ciphers:aes-256-gcm-nopadding:0';
const aes128GcmCipher = 'urn:xmpp:ciphers:aes-128-gcm-nopadding:0';
const maxAttachmentBytes = 100 * 1024 * 1024;
// The discovery RPC currently returns the upload JID, not its size limit.
// This matches the Waddle server's default limit on the transmitted body.
const maxUploadBytes = 10 * 1024 * 1024;
const _gcmTagBytes = 16;

Future<void> _pickerTail = Future.value();

Future<T> _serializePicker<T>(Future<T> Function() operation) async {
  final previous = _pickerTail;
  final done = Completer<void>();
  _pickerTail = done.future;
  try {
    await previous;
    return await operation();
  } finally {
    done.complete();
  }
}

Future<T> _withPickedCopies<T>(
  List<PlatformFile> files,
  Future<T> Function() operation,
) async {
  T result;
  try {
    result = await operation();
  } catch (error, stack) {
    await removePickedCopies(files);
    Error.throwWithStackTrace(error, stack);
  }
  if (!await removePickedCopies(files)) {
    throw StateError('Could not remove temporary selected files.');
  }
  return result;
}

/// XEP-0448 uses ciphertext followed by the 16-byte GCM authentication tag.
Future<Uint8List> decryptAttachment(
  Uint8List ciphertext,
  Map<String, dynamic> encrypted, {
  int? declaredSize,
}) async {
  final cipher = encrypted['cipher'];
  final length = cipher == aes256GcmCipher
      ? 32
      : cipher == aes128GcmCipher
      ? 16
      : 0;
  if (length == 0) {
    throw StateError('This encrypted attachment uses an unsupported cipher.');
  }
  Uint8List key, iv;
  try {
    key = base64Decode(encrypted['key_b64'] as String);
    iv = base64Decode(encrypted['iv_b64'] as String);
  } catch (_) {
    throw StateError('The encrypted attachment has invalid key material.');
  }
  if (key.length != length || iv.length != 12 || ciphertext.length < 16) {
    throw StateError('The encrypted attachment has invalid key material.');
  }
  await verifyHashes(ciphertext, runtimeRows(encrypted['hashes']));
  final algorithm = length == 32 ? AesGcm.with256bits() : AesGcm.with128bits();
  final plain = await algorithm.decrypt(
    SecretBox(
      ciphertext.sublist(0, ciphertext.length - 16),
      nonce: iv,
      mac: Mac(ciphertext.sublist(ciphertext.length - 16)),
    ),
    secretKey: SecretKey(key),
  );
  if (declaredSize != null &&
      declaredSize >= 0 &&
      declaredSize < plain.length) {
    return Uint8List.fromList(plain.sublist(0, declaredSize));
  }
  return Uint8List.fromList(plain);
}

Future<void> verifyHashes(
  List<int> bytes,
  List<Map<String, dynamic>> hashes,
) async {
  final supported = hashes
      .where((hash) => hash['algo']?.toString().toLowerCase() == 'sha-256')
      .toList();
  if (supported.isEmpty) {
    return;
  }
  final actual = (await Sha256().hash(bytes)).bytes;
  for (final hash in supported) {
    try {
      final expected = base64Decode(hash['value_b64'] as String);
      if (expected.length != actual.length) {
        continue;
      }
      var difference = 0;
      for (var i = 0; i < actual.length; i++) {
        difference |= actual[i] ^ expected[i];
      }
      if (difference == 0) {
        return;
      }
    } catch (_) {
      /* An invalid advertised digest cannot match. */
    }
  }
  throw StateError('The attachment failed its integrity check.');
}

Uri secureAttachmentUrl(dynamic value) {
  final url = Uri.tryParse(value?.toString() ?? '');
  if (url == null ||
      url.scheme != 'https' ||
      url.host.isEmpty ||
      url.userInfo.isNotEmpty) {
    throw StateError('Attachment transfers require an HTTPS URL.');
  }
  return url;
}

class PlatformFileService {
  PlatformFileService(
    this.context, {
    http.Client? client,
    this.downloadTimeout = const Duration(seconds: 45),
  }) : _client = client ?? http.Client() {
    unawaited(preparePrivateFiles().catchError((Object _) {}));
  }
  final RuntimeContext context;
  final http.Client _client;
  final Duration downloadTimeout;
  final _downloads = <String, Future<Uint8List>>{};
  final _activeDownloads = <_DownloadAttempt>{};
  bool _disposed = false;

  Future<dynamic> invoke(String method, Map<String, dynamic> args) async {
    switch (method) {
      case 'file.pickAndUpload':
        final picked = await _pick();
        if (picked == null) {
          return null;
        }
        return upload(picked);
      case 'file.uploadBytes':
        return upload(args);
      case 'file.uploadUri':
        final generation = context.generation;
        final bytes = await readLocalFileUri(
          args['uri'] as String,
          maxBytes: maxUploadBytes - _gcmTagBytes,
          validate: () => requireGeneration(context, generation),
        );
        requireGeneration(context, generation);
        return upload({...args, 'bytes': bytes});
      case 'file.pasteAndUpload':
        final generation = context.generation;
        final image = await readClipboardImage();
        requireGeneration(context, generation);
        if (image == null) {
          return null;
        }
        final mediaType = _mime(
          'image',
          image.bytes,
          providerType: image.mimeType,
        );
        return upload({
          'bytes': image.bytes,
          'name': _clipboardName(mediaType),
          'media_type': mediaType,
        });
      case 'file.bytes':
        return bytes(runtimeMap(args['file']));
      case 'file.open':
        final generation = context.generation;
        final file = runtimeMap(args['file']);
        final value = await bytes(file);
        requireGeneration(context, generation);
        await openPrivateFile(
          value,
          file['name']?.toString() ?? 'attachment',
          file['media_type']?.toString() ?? 'application/octet-stream',
          validate: () => requireGeneration(context, generation),
        );
        return null;
      case 'profile.pickAvatar':
      case 'image.avatar':
        final generation = context.generation;
        final picked = await _pick(images: true);
        if (picked == null) {
          return null;
        }
        final image = await processImage(
          _asBytes(picked['bytes']),
          square: true,
        );
        requireGeneration(context, generation);
        return {...image, 'data': image['bytes']};
      case 'file.pickImages':
        return _pickImages();
      case 'stickers.prepare':
        return _prepareStickers(args);
      default:
        throw UnsupportedError('Unsupported platform file operation: $method');
    }
  }

  Future<Map<String, dynamic>?> _pick({bool images = false}) {
    final generation = context.generation;
    return _serializePicker(() async {
      requireGeneration(context, generation);
      final file = await FilePicker.pickFile(
        type: images ? FileType.image : FileType.any,
        androidOptions: pickedFileAndroidOptions,
      );
      final picked = await _withPickedCopies(
        file == null ? [] : [file],
        () async {
          requireGeneration(context, generation);
          if (file == null) return null;
          final providerType = (await pickedFileProviderTypes([file])).single;
          requireGeneration(context, generation);
          return _readPicked(
            file,
            generation,
            providerType: providerType,
            maxBytes: images
                ? maxAttachmentBytes
                : maxUploadBytes - _gcmTagBytes,
          );
        },
      );
      requireGeneration(context, generation);
      return picked;
    });
  }

  Future<Map<String, dynamic>> _readPicked(
    PlatformFile file,
    int generation, {
    int maxBytes = maxAttachmentBytes,
    String? providerType,
  }) async {
    if ((await file.length() ?? 0) > maxBytes) {
      throw StateError('The selected attachment is too large.');
    }
    requireGeneration(context, generation);
    final builder = BytesBuilder(copy: false);
    await for (final chunk in file.readAsByteStream()) {
      requireGeneration(context, generation);
      if (builder.length + chunk.length > maxBytes) {
        throw StateError('The selected attachment is too large.');
      }
      builder.add(chunk);
    }
    final bytes = builder.takeBytes();
    requireGeneration(context, generation);
    return {
      'bytes': bytes,
      'name': file.name,
      'media_type': _mime(
        file.name,
        bytes,
        providerType: _validProviderType(providerType) ?? file.xFile.mimeType,
      ),
    };
  }

  Future<List<Map<String, dynamic>>> _pickImages() {
    final generation = context.generation;
    return _serializePicker(() async {
      requireGeneration(context, generation);
      final selection = await FilePicker.pickFiles(
        type: FileType.image,
        androidOptions: pickedFileAndroidOptions,
      );
      final result = await _withPickedCopies(selection, () async {
        requireGeneration(context, generation);
        if (selection.length > 100) {
          throw StateError('Choose at most 100 sticker images at a time.');
        }
        final providerTypes = await pickedFileProviderTypes(selection);
        requireGeneration(context, generation);
        final result = <Map<String, dynamic>>[];
        for (var i = 0; i < selection.length; i++) {
          result.add(
            await _readPicked(
              selection[i],
              generation,
              providerType: providerTypes[i],
            ),
          );
        }
        requireGeneration(context, generation);
        return result;
      });
      requireGeneration(context, generation);
      return result;
    });
  }

  Future<Map<String, dynamic>> upload(
    Map<String, dynamic> input, {
    bool encrypt = true,
  }) async {
    if (_disposed) {
      throw StateError('File service is closed.');
    }
    final generation = context.generation;
    final plain = _asBytes(input['bytes'] ?? input['data']);
    if (plain.length + (encrypt ? _gcmTagBytes : 0) > maxUploadBytes) {
      throw StateError('The attachment exceeds the 10 MiB upload limit.');
    }
    final name = input['name']?.toString() ?? 'attachment';
    final mediaType = _mime(
      name,
      plain,
      providerType: input['media_type']?.toString(),
    );
    final service = await context.rpc('discover_upload_service');
    requireGeneration(context, generation);
    if (service is! String || service.isEmpty) {
      throw StateError('This server does not advertise a file upload service.');
    }
    Map<String, dynamic>? encrypted;
    var transmitted = plain;
    if (encrypt) {
      final algorithm = AesGcm.with256bits();
      final key = await algorithm.newSecretKey();
      final nonce = algorithm.newNonce();
      final box = await algorithm.encrypt(plain, secretKey: key, nonce: nonce);
      transmitted = Uint8List.fromList([...box.cipherText, ...box.mac.bytes]);
      encrypted = {
        'cipher': aes256GcmCipher,
        'key_b64': base64Encode(await key.extractBytes()),
        'iv_b64': base64Encode(nonce),
        'hashes': [
          {
            'algo': 'sha-256',
            'value_b64': base64Encode((await Sha256().hash(transmitted)).bytes),
          },
        ],
      };
    }
    requireGeneration(context, generation);
    final slot = runtimeMap(
      await context.rpc('request_upload_slot', {
        'service_jid': service,
        'filename': encrypt ? '${_nonce()}.bin' : name,
        'size': transmitted.length,
        'content_type': encrypt ? 'application/octet-stream' : mediaType,
      }),
    );
    requireGeneration(context, generation);
    final put = secureAttachmentUrl(slot['put_url']);
    final get = secureAttachmentUrl(slot['get_url']);
    final request = http.Request('PUT', put)
      ..followRedirects = false
      ..bodyBytes = transmitted;
    request.headers['Content-Type'] = encrypt
        ? 'application/octet-stream'
        : mediaType;
    for (final header in runtimeRows(slot['put_headers'])) {
      final key = header['name']?.toString() ?? '';
      final value = header['value']?.toString() ?? '';
      if (!{'authorization', 'cookie', 'expires'}.contains(key.toLowerCase()) ||
          value.contains(RegExp(r'[\r\n]'))) {
        throw StateError('The server returned an invalid upload header.');
      }
      request.headers[key] = value;
    }
    final response = await _client
        .send(request)
        .timeout(const Duration(minutes: 2));
    await response.stream.drain<void>();
    requireGeneration(context, generation);
    if (response.statusCode < 200 || response.statusCode >= 300) {
      throw StateError('The attachment upload failed.');
    }
    if (encrypted != null) {
      encrypted['sources'] = [get.toString()];
    }
    final plainDigest = base64Encode((await Sha256().hash(plain)).bytes);
    requireGeneration(context, generation);
    return {
      'url': get.toString(),
      'name': name,
      'media_type': mediaType,
      'size': plain.length,
      if (input['width'] != null) 'width': input['width'],
      if (input['height'] != null) 'height': input['height'],
      'hashes': [
        {'algo': 'sha-256', 'value_b64': plainDigest},
      ],
      'disposition': mediaType.startsWith('image/') ? 'inline' : 'attachment',
      'encrypted': ?encrypted,
    };
  }

  Future<Uint8List> bytes(Map<String, dynamic> file) async {
    if (_disposed) {
      throw StateError('File service is closed.');
    }
    final generation = context.generation;
    // Complete envelopes form the key: ciphertext URLs never alias different keys/IVs.
    final key = '$generation|${jsonEncode(file)}';
    final existing = _downloads[key];
    if (existing != null) {
      return existing;
    }
    final pending = _download(file, generation);
    _downloads[key] = pending;
    try {
      return await pending;
    } finally {
      if (_downloads[key] == pending) {
        _downloads.remove(key);
      }
    }
  }

  Future<Uint8List> _download(Map<String, dynamic> file, int generation) async {
    final encryption = runtimeMap(file['encrypted']);
    final sources = encryption.isEmpty
        ? [file['url']]
        : (encryption['sources'] as List?) ?? [file['url']];
    for (final source in sources) {
      try {
        requireGeneration(context, generation);
        final attempt = _DownloadAttempt(generation);
        _activeDownloads.add(attempt);
        Uint8List value;
        try {
          value = await attempt.run(
            _client,
            secureAttachmentUrl(source),
            downloadTimeout,
            () => requireGeneration(context, generation),
          );
        } finally {
          _activeDownloads.remove(attempt);
        }
        requireGeneration(context, generation);
        if (encryption.isNotEmpty) {
          value = await decryptAttachment(
            value,
            encryption,
            declaredSize: (file['size'] as num?)?.toInt(),
          );
        }
        await verifyHashes(value, runtimeRows(file['hashes']));
        requireGeneration(context, generation);
        return value;
      } on AccountChanged {
        rethrow;
      } catch (_) {
        requireGeneration(context, generation);
        /* Each XEP-0448 source is an independent verified fallback. */
      }
    }
    throw StateError('This attachment could not be downloaded or verified.');
  }

  Future<Map<String, dynamic>?> _prepareStickers(
    Map<String, dynamic> args,
  ) async {
    final generation = context.generation;
    final name = args['name']?.toString().trim() ?? '';
    if (name.isEmpty) {
      throw StateError('Give the sticker pack a name.');
    }
    final picked = await _pickImages();
    if (picked.isEmpty) {
      return null;
    }
    final stickers = <Map<String, dynamic>>[];
    for (final pick in picked) {
      final processed = await processImage(_asBytes(pick['bytes']));
      requireGeneration(context, generation);
      final file = await upload({
        ...processed,
        'name': '${_nonce()}.png',
      }, encrypt: false);
      requireGeneration(context, generation);
      stickers.add({
        'desc': name,
        'media_type': 'image/png',
        'size': file['size'],
        'width': processed['width'],
        'height': processed['height'],
        'hashes': file['hashes'],
        'sources': [file['url']],
        'suggests': <String>[],
      });
    }
    return {
      'id': '',
      'name': name,
      'summary': args['summary'],
      'restricted': false,
      'stickers': stickers,
    };
  }

  Future<void> onAccountChanged() async {
    _downloads.clear();
    for (final attempt in _activeDownloads.toList()) {
      if (attempt.generation != context.generation || _disposed) {
        attempt.cancel(AccountChanged());
      }
    }
    await clearPrivateFiles();
  }

  Future<void> dispose() async {
    _disposed = true;
    _client.close();
    await onAccountChanged();
  }
}

class _DownloadAttempt {
  _DownloadAttempt(this.generation);

  final int generation;
  final _abort = Completer<void>();
  final _result = Completer<Uint8List>();
  StreamSubscription<List<int>>? _subscription;

  void cancel(Object error, [StackTrace? stackTrace]) {
    if (!_result.isCompleted) {
      _result.completeError(error, stackTrace);
    }
    if (!_abort.isCompleted) {
      _abort.complete();
    }
    final subscription = _subscription;
    if (subscription != null) {
      unawaited(subscription.cancel().catchError((Object _) {}));
    }
  }

  Future<Uint8List> run(
    http.Client client,
    Uri url,
    Duration timeout,
    void Function() validate,
  ) async {
    final timer = Timer(
      timeout,
      () => cancel(TimeoutException('Attachment download timed out.', timeout)),
    );
    final request = http.AbortableRequest(
      'GET',
      url,
      abortTrigger: _abort.future,
    )..followRedirects = false;
    unawaited(() async {
      try {
        final response = await client.send(request);
        if (_result.isCompleted ||
            response.statusCode != 200 ||
            (response.contentLength ?? 0) > maxAttachmentBytes + _gcmTagBytes) {
          final subscription = response.stream.listen(
            (_) {},
            onError: (Object _, StackTrace _) {},
          );
          unawaited(subscription.cancel().catchError((Object _) {}));
          cancel(StateError('The attachment source could not be read.'));
          return;
        }
        final builder = BytesBuilder(copy: false);
        _subscription = response.stream.listen(
          (chunk) {
            try {
              validate();
              if (builder.length + chunk.length >
                  maxAttachmentBytes + _gcmTagBytes) {
                throw StateError('The attachment is too large.');
              }
              builder.add(chunk);
            } catch (error, stackTrace) {
              cancel(error, stackTrace);
            }
          },
          onError: (Object error, StackTrace stackTrace) {
            cancel(error, stackTrace);
          },
          onDone: () {
            try {
              validate();
              if (!_result.isCompleted) _result.complete(builder.takeBytes());
            } catch (error, stackTrace) {
              cancel(error, stackTrace);
            }
          },
        );
        if (_result.isCompleted) {
          unawaited(_subscription!.cancel().catchError((Object _) {}));
        }
      } catch (error, stackTrace) {
        cancel(error, stackTrace);
      }
    }());
    try {
      return await _result.future;
    } finally {
      timer.cancel();
    }
  }
}

Future<Map<String, dynamic>> processImage(
  Uint8List bytes, {
  bool square = false,
}) async {
  if (bytes.length > maxAttachmentBytes) {
    throw StateError('This image is too large.');
  }
  final buffer = await ui.ImmutableBuffer.fromUint8List(bytes);
  ui.ImageDescriptor? descriptor;
  ui.Codec? codec;
  ui.Image? image;
  try {
    descriptor = await ui.ImageDescriptor.encoded(buffer);
    final scale = min(1.0, 1024 / max(descriptor.width, descriptor.height));
    codec = await descriptor.instantiateCodec(
      targetWidth: max(1, (descriptor.width * scale).round()),
      targetHeight: max(1, (descriptor.height * scale).round()),
    );
    image = (await codec.getNextFrame()).image;
    final png = await image.toByteData(format: ui.ImageByteFormat.png);
    if (png == null) {
      throw StateError('This image could not be read.');
    }
    var decoded = img.decodePng(png.buffer.asUint8List())!;
    if (square) {
      final edge = min(decoded.width, decoded.height);
      decoded = img.copyCrop(
        decoded,
        x: (decoded.width - edge) ~/ 2,
        y: (decoded.height - edge) ~/ 2,
        width: edge,
        height: edge,
      );
    }
    if (max(decoded.width, decoded.height) > 512) {
      final ratio = 512 / max(decoded.width, decoded.height);
      decoded = img.copyResize(
        decoded,
        width: max(1, (decoded.width * ratio).round()),
        height: max(1, (decoded.height * ratio).round()),
      );
    }
    return {
      'bytes': Uint8List.fromList(img.encodePng(decoded)),
      'width': decoded.width,
      'height': decoded.height,
      'media_type': 'image/png',
    };
  } finally {
    image?.dispose();
    codec?.dispose();
    descriptor?.dispose();
    buffer.dispose();
  }
}

Uint8List _asBytes(dynamic value) {
  if (value is Uint8List) {
    return value;
  }
  if (value is List) {
    return Uint8List.fromList(value.cast<int>());
  }
  throw ArgumentError('The platform did not provide attachment bytes.');
}

String _nonce() => List.generate(
  16,
  (_) => Random.secure().nextInt(256).toRadixString(16).padLeft(2, '0'),
).join();
final _mimePattern = RegExp(
  r'^[a-z0-9][a-z0-9!#$&^_.+\-]{0,126}/[a-z0-9][a-z0-9!#$&^_.+\-]{0,126}$',
);

String? _validProviderType(String? value) {
  final type = value?.trim().toLowerCase();
  if (type == null ||
      type == 'application/octet-stream' ||
      !_mimePattern.hasMatch(type)) {
    return null;
  }
  return type;
}

String _mime(String name, Uint8List bytes, {String? providerType}) {
  final provided = _validProviderType(providerType);
  if (provided != null) return provided;
  bool startsWith(List<int> signature, [int offset = 0]) =>
      bytes.length >= offset + signature.length &&
      signature.asMap().entries.every(
        (entry) => bytes[offset + entry.key] == entry.value,
      );
  if (startsWith([0xff, 0xd8, 0xff])) return 'image/jpeg';
  if (startsWith([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])) {
    return 'image/png';
  }
  if (startsWith(ascii.encode('GIF87a')) ||
      startsWith(ascii.encode('GIF89a'))) {
    return 'image/gif';
  }
  if (startsWith(ascii.encode('RIFF')) && startsWith(ascii.encode('WEBP'), 8)) {
    return 'image/webp';
  }
  if (startsWith(ascii.encode('ftyp'), 4) && bytes.length >= 12) {
    final boxSize = ByteData.sublistView(bytes, 0, 4).getUint32(0);
    final end = min(bytes.length, min(boxSize, 256));
    final brands = <String>{
      ascii.decode(bytes.sublist(8, 12), allowInvalid: true),
    };
    for (var offset = 16; offset + 4 <= end; offset += 4) {
      brands.add(
        ascii.decode(bytes.sublist(offset, offset + 4), allowInvalid: true),
      );
    }
    if (brands.any({'avif', 'avis'}.contains)) return 'image/avif';
    if (brands.any({'heic', 'heix', 'hevc', 'hevx'}.contains)) {
      return 'image/heic';
    }
    if (brands.any({'mif1', 'msf1', 'heim', 'heis', 'hevm', 'hevs'}.contains)) {
      return 'image/heif';
    }
  }
  return switch (name.toLowerCase().split('.').last) {
    'png' => 'image/png',
    'jpg' || 'jpeg' => 'image/jpeg',
    'webp' => 'image/webp',
    'gif' => 'image/gif',
    'heic' => 'image/heic',
    'heif' => 'image/heif',
    'avif' => 'image/avif',
    'bmp' => 'image/bmp',
    'tif' || 'tiff' => 'image/tiff',
    'svg' => 'image/svg+xml',
    'pdf' => 'application/pdf',
    'txt' || 'log' => 'text/plain',
    'csv' => 'text/csv',
    'html' || 'htm' => 'text/html',
    'json' => 'application/json',
    'zip' => 'application/zip',
    'doc' => 'application/msword',
    'docx' =>
      'application/vnd.openxmlformats-officedocument.wordprocessingml.document',
    'xls' => 'application/vnd.ms-excel',
    'xlsx' =>
      'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet',
    'ppt' => 'application/vnd.ms-powerpoint',
    'pptx' =>
      'application/vnd.openxmlformats-officedocument.presentationml.presentation',
    'mp4' => 'video/mp4',
    'mov' => 'video/quicktime',
    'webm' => 'video/webm',
    'mp3' => 'audio/mpeg',
    'm4a' => 'audio/mp4',
    'wav' => 'audio/wav',
    'ogg' || 'oga' => 'audio/ogg',
    'opus' => 'audio/opus',
    'flac' => 'audio/flac',
    _ => 'application/octet-stream',
  };
}

String _clipboardName(String mediaType) => switch (mediaType) {
  'image/jpeg' => 'image.jpg',
  'image/png' => 'image.png',
  'image/gif' => 'image.gif',
  'image/webp' => 'image.webp',
  'image/heic' => 'image.heic',
  'image/heif' => 'image.heif',
  'image/avif' => 'image.avif',
  _ => 'image',
};

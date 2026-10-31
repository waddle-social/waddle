import 'dart:async';
import 'dart:convert';
import 'dart:math';
import 'dart:typed_data';
import 'dart:ui' as ui;

import 'package:cryptography/cryptography.dart';
import 'package:file_picker/file_picker.dart';
import 'package:http/http.dart' as http;
import 'package:image/image.dart' as img;
import 'package:pasteboard/pasteboard.dart';

import 'context.dart';
import 'platform_file_access.dart';

const aes256GcmCipher = 'urn:xmpp:ciphers:aes-256-gcm-nopadding:0';
const aes128GcmCipher = 'urn:xmpp:ciphers:aes-128-gcm-nopadding:0';
const maxAttachmentBytes = 100 * 1024 * 1024;

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
  PlatformFileService(this.context, {http.Client? client})
    : _client = client ?? http.Client() {
    unawaited(preparePrivateFiles().catchError((Object _) {}));
  }
  final RuntimeContext context;
  final http.Client _client;
  final _downloads = <String, Future<Uint8List>>{};
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
        final bytes = await readLocalFileUri(args['uri'] as String);
        requireGeneration(context, generation);
        return upload({...args, 'bytes': bytes});
      case 'file.pasteAndUpload':
        final generation = context.generation;
        final bytes = await Pasteboard.image;
        requireGeneration(context, generation);
        if (bytes == null) {
          return null;
        }
        return upload({
          'bytes': bytes,
          'name': 'image.png',
          'media_type': 'image/png',
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

  Future<Map<String, dynamic>?> _pick({bool images = false}) async {
    final generation = context.generation;
    final file = await FilePicker.pickFile(
      type: images ? FileType.image : FileType.any,
    );
    requireGeneration(context, generation);
    if (file == null) {
      return null;
    }
    return _readPicked(file, generation);
  }

  Future<Map<String, dynamic>> _readPicked(
    PlatformFile file,
    int generation,
  ) async {
    if ((await file.length() ?? 0) > maxAttachmentBytes) {
      throw StateError('The selected attachment is too large.');
    }
    requireGeneration(context, generation);
    final builder = BytesBuilder(copy: false);
    await for (final chunk in file.readAsByteStream()) {
      requireGeneration(context, generation);
      if (builder.length + chunk.length > maxAttachmentBytes) {
        throw StateError('The selected attachment is too large.');
      }
      builder.add(chunk);
    }
    return {
      'bytes': builder.takeBytes(),
      'name': file.name,
      'media_type': _mime(file.name),
    };
  }

  Future<List<Map<String, dynamic>>> _pickImages() async {
    final generation = context.generation;
    final selection = await FilePicker.pickFiles(type: FileType.image);
    requireGeneration(context, generation);
    if (selection.length > 100) {
      throw StateError('Choose at most 100 sticker images at a time.');
    }
    final result = <Map<String, dynamic>>[];
    for (final file in selection) {
      result.add(await _readPicked(file, generation));
    }
    requireGeneration(context, generation);
    return result;
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
    if (plain.length > maxAttachmentBytes) {
      throw StateError('The attachment exceeds the 100 MB limit.');
    }
    final name = input['name']?.toString() ?? 'attachment';
    final mediaType = input['media_type']?.toString() ?? _mime(name);
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
        final request = http.Request('GET', secureAttachmentUrl(source))
          ..followRedirects = false;
        final response = await _client
            .send(request)
            .timeout(const Duration(seconds: 45));
        if (response.statusCode != 200 ||
            (response.contentLength ?? 0) > maxAttachmentBytes + 16) {
          await response.stream.drain<void>();
          continue;
        }
        final builder = BytesBuilder(copy: false);
        await for (final chunk in response.stream) {
          requireGeneration(context, generation);
          if (builder.length + chunk.length > maxAttachmentBytes + 16) {
            throw StateError('The attachment is too large.');
          }
          builder.add(chunk);
        }
        requireGeneration(context, generation);
        var value = builder.takeBytes();
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
    await clearPrivateFiles();
  }

  Future<void> dispose() async {
    _disposed = true;
    _client.close();
    await onAccountChanged();
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
String _mime(String name) => switch (name.toLowerCase().split('.').last) {
  'png' => 'image/png',
  'jpg' || 'jpeg' => 'image/jpeg',
  'webp' => 'image/webp',
  'gif' => 'image/gif',
  'pdf' => 'application/pdf',
  'txt' => 'text/plain',
  'mp4' => 'video/mp4',
  'mp3' => 'audio/mpeg',
  _ => 'application/octet-stream',
};

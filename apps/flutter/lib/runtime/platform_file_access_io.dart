import 'dart:io';
import 'dart:math';
import 'dart:typed_data';

import 'package:open_filex/open_filex.dart';
import 'package:path_provider/path_provider.dart';

Future<Directory>? _ready;
Future<void> _cleanup = Future.value();
Future<Directory> _directory() {
  final existing = _ready;
  if (existing != null) return existing;
  final previousCleanup = _cleanup;
  return _ready = () async {
    await previousCleanup;
    final root = await getTemporaryDirectory();
    final directory = Directory('${root.path}/waddle-open');
    if (await directory.exists()) {
      await directory.delete(recursive: true);
    }
    await directory.create(recursive: true);
    return directory;
  }();
}

Future<void> openPrivateFile(
  Uint8List bytes,
  String name,
  String mediaType, {
  required void Function() validate,
}) async {
  final directory = await _directory();
  validate();
  final safe = name
      .split(RegExp(r'[/\\]'))
      .last
      .replaceAll(RegExp(r'[^\w. -]'), '_');
  final suffix = Random.secure().nextInt(1 << 32).toRadixString(16);
  final file = File(
    '${directory.path}/$suffix-${safe.isEmpty ? 'attachment' : safe}',
  );
  await file.writeAsBytes(bytes, flush: true);
  try {
    validate();
  } catch (_) {
    await file.delete();
    rethrow;
  }
  final result = await OpenFilex.open(file.path, type: mediaType);
  if (result.type != ResultType.done) {
    throw StateError('No application could open this attachment.');
  }
}

Future<Uint8List> readLocalFileUri(
  String value, {
  required int maxBytes,
  void Function()? validate,
}) async {
  final uri = Uri.parse(value);
  if (uri.scheme.isNotEmpty && uri.scheme != 'file') {
    throw UnsupportedError(
      'This keyboard did not provide image bytes. Use the attachment picker.',
    );
  }
  final file = uri.scheme == 'file' ? File.fromUri(uri) : File(value);
  if (await file.length() > maxBytes) {
    throw StateError('This attachment is too large.');
  }
  final bytes = BytesBuilder(copy: false);
  await for (final chunk in file.openRead()) {
    validate?.call();
    if (bytes.length + chunk.length > maxBytes) {
      throw StateError('This attachment is too large.');
    }
    bytes.add(chunk);
  }
  validate?.call();
  return bytes.takeBytes();
}

Future<void> clearPrivateFiles() {
  final previous = _ready;
  final previousCleanup = _cleanup;
  _ready = null;
  final cleanup = () async {
    await previousCleanup;
    Directory? cache;
    try {
      if (previous != null) cache = await previous;
    } catch (_) {
      /* No cache was created. */
    }
    if (cache != null && await cache.exists()) {
      await cache.delete(recursive: true);
    }
  }();
  _cleanup = cleanup.catchError((Object _) {});
  return cleanup;
}

Future<void> preparePrivateFiles() async {
  await _directory();
}

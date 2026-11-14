import 'dart:io';
import 'dart:math';
import 'dart:typed_data';

import 'package:open_filex/open_filex.dart';
import 'package:path_provider/path_provider.dart';

Directory? _cache;
Future<Directory>? _ready;
Future<Directory> _directory() => _ready ??= () async {
  final root = await getTemporaryDirectory();
  final directory = Directory('${root.path}/waddle-open');
  if (await directory.exists()) {
    await directory.delete(recursive: true);
  }
  await directory.create(recursive: true);
  return _cache = directory;
}();

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

Future<Uint8List> readLocalFileUri(String value) async {
  final uri = Uri.parse(value);
  if (uri.scheme.isNotEmpty && uri.scheme != 'file') {
    throw UnsupportedError(
      'This keyboard did not provide image bytes. Use the attachment picker.',
    );
  }
  final file = uri.scheme == 'file' ? File.fromUri(uri) : File(value);
  if (await file.length() > 100 * 1024 * 1024) {
    throw StateError('This attachment is too large.');
  }
  return file.readAsBytes();
}

Future<void> clearPrivateFiles() async {
  if (_ready != null) {
    try {
      await _ready;
    } catch (_) {
      /* No cache was created. */
    }
  }
  final cache = _cache;
  _cache = null;
  _ready = null;
  if (cache != null && await cache.exists()) {
    await cache.delete(recursive: true);
  }
}

Future<void> preparePrivateFiles() async {
  await _directory();
}

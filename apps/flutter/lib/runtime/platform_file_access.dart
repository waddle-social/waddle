import 'dart:typed_data';

import 'platform_file_access_stub.dart'
    if (dart.library.io) 'platform_file_access_io.dart'
    if (dart.library.js_interop) 'platform_file_access_web.dart'
    as platform;

Future<void> openPrivateFile(
  Uint8List bytes,
  String name,
  String mediaType, {
  required void Function() validate,
}) => platform.openPrivateFile(bytes, name, mediaType, validate: validate);
Future<Uint8List> readLocalFileUri(String uri) =>
    platform.readLocalFileUri(uri);
Future<void> clearPrivateFiles() => platform.clearPrivateFiles();

Future<void> preparePrivateFiles() => platform.preparePrivateFiles();

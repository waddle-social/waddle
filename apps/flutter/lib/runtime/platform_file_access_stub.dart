import 'dart:typed_data';

Future<void> openPrivateFile(
  Uint8List bytes,
  String name,
  String mediaType, {
  required void Function() validate,
}) => Future.error(
  UnsupportedError('File viewing is unavailable on this platform.'),
);
Future<Uint8List> readLocalFileUri(String uri) => Future.error(
  UnsupportedError('Local file access is unavailable on this platform.'),
);
Future<void> clearPrivateFiles() async {}

Future<void> preparePrivateFiles() async {}

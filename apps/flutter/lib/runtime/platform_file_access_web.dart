import 'dart:async';
import 'dart:js_interop';
import 'dart:typed_data';
import 'package:web/web.dart' as web;

final _objectUrls = <String>{};
Future<void> openPrivateFile(
  Uint8List bytes,
  String name,
  String mediaType, {
  required void Function() validate,
}) async {
  validate();
  final blob = web.Blob(
    [bytes.toJS].toJS,
    web.BlobPropertyBag(type: mediaType),
  );
  final url = web.URL.createObjectURL(blob);
  _objectUrls.add(url);
  final anchor = web.HTMLAnchorElement()
    ..href = url
    ..download = name;
  web.document.body?.append(anchor);
  anchor.click();
  anchor.remove();
  Timer(const Duration(minutes: 1), () {
    if (_objectUrls.remove(url)) {
      web.URL.revokeObjectURL(url);
    }
  });
}

Future<Uint8List> readLocalFileUri(String uri) => Future.error(
  UnsupportedError(
    'The browser must provide the dropped or pasted file bytes.',
  ),
);
Future<void> clearPrivateFiles() async {
  for (final url in _objectUrls) {
    web.URL.revokeObjectURL(url);
  }
  _objectUrls.clear();
}

Future<void> preparePrivateFiles() async {}

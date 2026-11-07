import 'dart:async';

import 'package:android_file_picker/android_file_picker.dart';
import 'package:file_picker/file_picker.dart';
import 'package:flutter/foundation.dart'
    show TargetPlatform, defaultTargetPlatform, kIsWeb;
import 'package:flutter/services.dart';
import 'package:pasteboard/pasteboard.dart';

const pickedFileAndroidOptions = FilePickerAndroidOptions(
  safOptions: AndroidSAFOptions(
    grant: AndroidSAFGrant.transient,
    accessMode: AndroidSAFAccessMode.readOnly,
    persistGrant: false,
  ),
);

const _channel = MethodChannel('social.waddle.flutter/platform_files');

Future<({Uint8List bytes, String? mimeType})?> readClipboardImage() async {
  if (!kIsWeb && defaultTargetPlatform == TargetPlatform.android) {
    try {
      final image = await _channel.invokeMapMethod<String, Object?>(
        'clipboardImage',
      );
      if (image == null) return null;
      final bytes = image['bytes'];
      if (bytes is! Uint8List) {
        throw StateError('Could not read the clipboard image.');
      }
      final type = image['mimeType'];
      return (bytes: bytes, mimeType: type is String ? type : null);
    } on PlatformException catch (error) {
      if (error.code == 'file_too_large') {
        throw StateError('The clipboard image exceeds the upload limit.');
      }
      throw StateError('Could not read the clipboard image.');
    } on MissingPluginException {
      throw StateError('Could not read the clipboard image.');
    }
  }
  final bytes = await Pasteboard.image;
  return bytes == null ? null : (bytes: bytes, mimeType: null);
}

/// Resolves provider types before the temporary Android URI grants expire.
Future<List<String?>> pickedFileProviderTypes(List<PlatformFile> files) async {
  final uris = files.map((file) {
    if (file is! AndroidPlatformFile) return null;
    final uri = file.safHandle?.uri;
    return uri != null && uri.scheme == 'content' && uri.hasAuthority
        ? uri.toString()
        : null;
  }).toList();
  final fallback = List<String?>.filled(files.length, null);
  if (uris.every((uri) => uri == null)) return fallback;
  try {
    final types = await _channel
        .invokeListMethod<Object?>('mimeTypes', uris)
        .timeout(const Duration(seconds: 5));
    if (types == null || types.length != files.length) return fallback;
    return types.map((type) => type is String ? type : null).toList();
  } on PlatformException {
    return fallback;
  } on MissingPluginException {
    return fallback;
  } on TimeoutException {
    return fallback;
  }
}

/// Removes only Android picker copies from the app's private cache.
Future<bool> removePickedCopies(List<PlatformFile> files) async {
  final paths = <String>[];
  var allHavePaths = true;
  for (final file in files) {
    if (file is AndroidPlatformFile) {
      final path = file.path;
      if (path == null) {
        allHavePaths = false;
      } else {
        paths.add(path);
      }
    }
  }
  if (paths.isEmpty) return allHavePaths;
  try {
    final removed = await _channel.invokeMethod<bool>(
      'removePickedCopies',
      paths,
    );
    return removed == true && allHavePaths;
  } on PlatformException {
    return false;
  } on MissingPluginException {
    return false;
  }
}

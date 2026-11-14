import 'dart:async';

import 'package:flutter/widgets.dart';
import 'package:url_launcher/url_launcher.dart';

import 'context.dart';
import 'platform_calls.dart';
import 'platform_files.dart';
import 'platform_media.dart';

export 'platform_media.dart' show CallMedia, LiveKitCallMedia;

/// Portable platform capabilities. Protocol and UI orchestration stay in Dart/Rust.
class PlatformServices {
  PlatformServices(
    this.context, {
    PlatformCallService? calls,
    PlatformFileService? files,
    CallMedia? media,
    Future<void> Function(bool)? requestCapture,
    Future<void> Function(bool active, bool video, String sid)? callForeground,
  }) : calls =
           calls ??
           PlatformCallService(
             context,
             media: media,
             requestCapture: requestCapture,
             callForeground: callForeground,
           ),
       files = files ?? PlatformFileService(context) {
    _subscription = this.calls.events.listen(
      _events.add,
      onError: _events.addError,
    );
  }
  final RuntimeContext context;
  final PlatformCallService calls;
  final PlatformFileService files;
  final _events = StreamController<Map<String, dynamic>>.broadcast(sync: true);
  late final StreamSubscription<Map<String, dynamic>> _subscription;
  Stream<Map<String, dynamic>> get events => _events.stream;

  Widget buildCallVideo(BuildContext context, {bool local = false}) =>
      calls.buildVideo(context, local: local);

  Future<dynamic> invoke(
    String method, [
    Map<String, dynamic> args = const {},
  ]) async {
    if (method.startsWith('call.')) {
      return calls.invoke(method, args);
    }
    if (method.startsWith('file.') ||
        {
          'profile.pickAvatar',
          'image.avatar',
          'stickers.prepare',
        }.contains(method)) {
      return files.invoke(method, args);
    }
    if (method == 'link.open' || method == 'url.open') {
      final url = Uri.tryParse(args['url']?.toString() ?? '');
      if (url == null ||
          !{'https', 'http', 'mailto'}.contains(url.scheme) ||
          !await launchUrl(url, mode: LaunchMode.externalApplication)) {
        throw StateError('This link could not be opened.');
      }
      return null;
    }
    throw UnsupportedError('Unsupported platform operation: $method');
  }

  void onEvent(Map<String, dynamic> event) => calls.onEvent(event);
  void onAccountChanged() {
    calls.onAccountChanged();
    unawaited(files.onAccountChanged());
  }

  Future<void> dispose() async {
    await _subscription.cancel();
    await calls.dispose();
    await files.dispose();
    await _events.close();
  }
}

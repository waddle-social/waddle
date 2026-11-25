import 'dart:io';
import 'dart:ui' as ui;

import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:flutter_webrtc/flutter_webrtc.dart' as rtc;
import 'package:integration_test/integration_test.dart';

/// The ATD virtual camera shows a changing scene without a person.
Future<double> _magentaFraction(rtc.MediaStreamTrack track) async {
  final bytes = await track.captureFrame().timeout(const Duration(seconds: 8));
  final codec = await ui.instantiateImageCodec(
    bytes.asUint8List(),
    targetWidth: 64,
  );
  try {
    final frame = await codec.getNextFrame();
    try {
      final rgba = (await frame.image.toByteData(
        format: ui.ImageByteFormat.rawRgba,
      ))!;
      var magenta = 0;
      for (var i = 0; i < rgba.lengthInBytes; i += 4) {
        if (rgba.getUint8(i) > 180 &&
            rgba.getUint8(i + 1) < 80 &&
            rgba.getUint8(i + 2) > 180) {
          magenta++;
        }
      }
      return magenta * 4 / rgba.lengthInBytes;
    } finally {
      frame.image.dispose();
    }
  } finally {
    codec.dispose();
  }
}

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  testWidgets('the camera sender applies and removes its native background', (
    tester,
  ) async {
    const channel = MethodChannel('social.waddle.flutter/camera_effects');
    final stream = await rtc.navigator.mediaDevices.getUserMedia({
      'audio': false,
      'video': {'width': 320, 'height': 240, 'frameRate': 15},
    });
    final track = stream.getVideoTracks().single;
    Future<void> set(String mode, [Map<String, Object> data = const {}]) =>
        channel.invokeMethod<void>('set', {
          'trackId': track.id,
          'mode': mode,
          ...data,
        });
    try {
      expect(await _magentaFraction(track), lessThan(.25));
      await set('blur');
      await set('image', {
        'rgba': Uint8List.fromList([255, 0, 255, 255]),
        'width': 1,
        'height': 1,
      });
      final deadline = Stopwatch()..start();
      var fraction = 0.0;
      while (fraction <= .55 && deadline.elapsed < const Duration(seconds: 8)) {
        await tester.pump(const Duration(milliseconds: 100));
        fraction = await _magentaFraction(track);
      }
      expect(fraction, greaterThan(.55));
      await set('off');
      expect(await _magentaFraction(track), lessThan(.25));
    } finally {
      await set('off');
      await track.stop();
      await stream.dispose();
    }
  }, skip: !Platform.isAndroid);
}

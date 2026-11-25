import 'dart:io';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:livekit_client/livekit_client.dart' as lk;
import 'package:waddle_flutter/runtime/call_screen_share.dart';

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  testWidgets(
    'captures a video frame from the isolated Wayland portal',
    (tester) async {
      expect(Platform.environment['DISPLAY'], isNull);
      await tester.pumpWidget(
        const MaterialApp(home: ColoredBox(color: Colors.green)),
      );
      final sharing = CallScreenShare();
      final room = lk.Room();
      final track = await sharing.capture(room, () => true);
      try {
        expect(track, isNotNull);
        final frame = await track!.mediaStreamTrack.captureFrame();
        final codec = await ui.instantiateImageCodec(frame.asUint8List());
        final image = (await codec.getNextFrame()).image;
        expect(image.width, greaterThan(100));
        expect(image.height, greaterThan(100));
        image.dispose();
        codec.dispose();
      } finally {
        await track?.stop();
        await track?.dispose();
        await sharing.release(room);
        await room.dispose();
        // Keep the process alive so the portal can observe capture teardown.
        await Future<void>.delayed(const Duration(seconds: 3));
      }
    },
    skip:
        !Platform.isLinux ||
        Platform.environment['WADDLE_ISOLATED_CAPTURE'] != '1',
  );
}

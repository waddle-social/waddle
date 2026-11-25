import 'dart:io';
import 'dart:convert';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:livekit_client/livekit_client.dart' as lk;
import 'package:flutter_webrtc/flutter_webrtc.dart' as rtc;
import 'package:waddle_flutter/runtime/call_screen_share.dart';
import 'package:waddle_flutter/runtime/story_capture_io.dart' as story;

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
      final trace = File(Platform.environment['WADDLE_CAPTURE_TRACE']!);
      try {
        for (var attempt = 1; attempt <= 2; attempt++) {
          final track = await sharing.capture(room, () => true);
          try {
            expect(track, isNotNull);
            final frame = await track!.mediaStreamTrack.captureFrame();
            final codec = await ui.instantiateImageCodec(frame.asUint8List());
            final image = (await codec.getNextFrame()).image;
            expect(image.width, 1280);
            expect(image.height, 720);
            image.dispose();
            codec.dispose();
            if (attempt == 1) {
              // Exercise the same native recorder used with a camera, using
              // only this compositor and the test's private Pulse monitor.
              final renderer = rtc.RTCVideoRenderer();
              final recording = await story.startVideo(
                track.mediaStream,
                renderer,
              );
              try {
                await Future<void>.delayed(const Duration(seconds: 3));
                final clip = await recording.finish();
                final atoms = latin1.decode(clip.bytes);
                for (final atom in ['ftyp', 'mdat', 'vide', 'soun']) {
                  expect(atoms, contains(atom));
                }
              } catch (_) {
                await recording.cancel();
                rethrow;
              }
            }
          } finally {
            await track?.stop();
            await track?.dispose();
            await sharing.release(room);
          }
          final deadline = DateTime.now().add(const Duration(seconds: 4));
          while (true) {
            final log = await trace.readAsString();
            final created = 'create session method invoked'
                .allMatches(log)
                .length;
            final closed = 'destroying session'.allMatches(log).length;
            final started = 'start method invoked'.allMatches(log).length;
            // Only the actual presentation should reach the source picker.
            expect(started, attempt);
            if (created == closed && created >= started) break;
            if (DateTime.now().isAfter(deadline)) {
              fail(
                'The portal still owns ${created - closed} capture sessions.',
              );
            }
            await Future<void>.delayed(const Duration(milliseconds: 50));
          }
        }
      } finally {
        await room.dispose();
      }
    },
    skip:
        !Platform.isLinux ||
        Platform.environment['WADDLE_ISOLATED_CAPTURE'] != '1',
  );
}

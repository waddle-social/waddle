import 'dart:convert';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:waddle_flutter/features/story_camera.dart';
import 'package:waddle_flutter/runtime/platform_files.dart' show maxUploadBytes;

/// Run with camera and microphone permission on an emulator with a virtual camera.
void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  testWidgets(
    'the device camera produces a decodable photo and an MP4 containing video and audio',
    (tester) async {
      final camera = await openStoryCamera(true);
      try {
        await tester.pumpWidget(
          MaterialApp(home: Scaffold(body: camera.preview)),
        );
        await tester.pump(const Duration(seconds: 2));
        final photo = await camera.photo().timeout(const Duration(seconds: 15));
        final codec = await ui.instantiateImageCodec(photo.bytes);
        final frame = await codec.getNextFrame();
        expect(frame.image.width, greaterThan(0));
        expect(frame.image.height, greaterThan(0));
        frame.image.dispose();
        codec.dispose();
        final recording = await camera.record().timeout(
          const Duration(seconds: 10),
        );
        try {
          await tester.pump(const Duration(seconds: 3));
          final clip = await recording.finish().timeout(
            const Duration(seconds: 20),
          );
          expect(clip.type, 'video/mp4');
          expect(clip.bytes.length, inInclusiveRange(1024, maxUploadBytes));
          final atoms = latin1.decode(clip.bytes);
          for (final name in ['ftyp', 'mdat', 'vide', 'soun']) {
            expect(atoms, contains(name));
          }
        } catch (_) {
          await recording.cancel();
          rethrow;
        }
      } finally {
        await camera.close();
      }
    },
  );
}

import 'dart:convert';
import 'dart:io';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:image/image.dart' as img;
import 'package:integration_test/integration_test.dart';
import 'package:livekit_client/livekit_client.dart' as lk;
import 'package:waddle_flutter/runtime/platform_media.dart';

Future<void> _expectRemoteCamera(
  lk.Room viewer,
  String mode,
  bool Function(double magenta, double luminance, double contrast) matches,
) async {
  final deadline = DateTime.now().add(const Duration(seconds: 25));
  String latest = 'waiting for a camera publication';
  while (DateTime.now().isBefore(deadline)) {
    final track = viewer.remoteParticipants.values
        .expand((p) => p.videoTrackPublications)
        .where((p) => p.source == lk.TrackSource.camera)
        .firstOrNull
        ?.track;
    if (track != null &&
        ((await track.getReceiverStats())?.framesDecoded ?? 0) > 0) {
      final png = await track.mediaStreamTrack.captureFrame();
      final codec = await ui.instantiateImageCodec(png.asUint8List());
      final image = (await codec.getNextFrame()).image;
      try {
        final rgba = (await image.toByteData(
          format: ui.ImageByteFormat.rawRgba,
        ))!;
        var magenta = 0, luma = 0, difference = 0, samples = 0;
        for (var y = 0; y < image.height; y += 8) {
          for (var x = 0; x + 1 < image.width; x += 8) {
            final i = (y * image.width + x) * 4;
            final r = rgba.getUint8(i),
                g = rgba.getUint8(i + 1),
                b = rgba.getUint8(i + 2);
            if (r > 180 && g < 80 && b > 180) magenta++;
            luma += g;
            difference += (g - rgba.getUint8(i + 5)).abs();
            samples++;
          }
        }
        final fraction = magenta / samples,
            mean = luma / samples,
            contrast = difference / samples;
        if (matches(fraction, mean, contrast)) return;
        latest = 'magenta=$fraction, mean=$mean, contrast=$contrast';
      } finally {
        image.dispose();
        codec.dispose();
      }
    }
    await Future<void>.delayed(const Duration(milliseconds: 150));
  }
  fail('The remote $mode camera did not match: $latest');
}

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  final path = Platform.environment['WADDLE_LIVEKIT_FIXTURE'];
  testWidgets(
    'a second participant decodes the shared Wayland display',
    (tester) async {
      final fixture = jsonDecode(await File(path!).readAsString()) as Map;
      final publisher = Map<String, dynamic>.from(fixture['publisher'] as Map);
      final viewer = Map<String, dynamic>.from(fixture['viewer'] as Map);
      if (fixture['cert'] case final String certificate) {
        SecurityContext.defaultContext.setTrustedCertificates(certificate);
      }
      await tester.pumpWidget(
        const MaterialApp(home: ColoredBox(color: Colors.green)),
      );
      final media = LiveKitCallMedia();
      final peer = lk.Room();
      try {
        await media.connect(publisher, false, []);
        await peer.connect(viewer['url'] as String, viewer['token'] as String);
        expect(media.snapshot['mediaState'], 'connected');
        await peer.localParticipant!.setMicrophoneEnabled(true);
        final audioDeadline = DateTime.now().add(const Duration(seconds: 15));
        while ((media.snapshot['audioTracks'] as List).isEmpty) {
          if (DateTime.now().isAfter(audioDeadline)) {
            fail('The publisher did not subscribe to the viewer microphone.');
          }
          await Future<void>.delayed(const Duration(milliseconds: 100));
        }
        final sid = media.snapshot['audioTracks'][0]['sid'] as String;
        await media.setPlaybackVolume(sid, 0.4);
        expect(media.snapshot['audioTracks'][0]['gain'], 0.4);
        await media.microphone(true);
        await media.setVoiceProcessing('noiseSuppression', false);
        final quality = await media.quality();
        expect(quality['canChangeVoice'], isTrue);
        expect(quality['voice']['noiseSuppression'], isFalse);
        expect(quality['streams'], isNotEmpty);
        await media.setVoiceProcessing('noiseSuppression', true);
        expect((await media.quality())['voice']['noiseSuppression'], isTrue);
        await media.screenShare(true);
        final deadline = DateTime.now().add(const Duration(seconds: 25));
        var bright = false;
        while (true) {
          // Supply desktop damage so the compositor delivers new frames.
          bright = !bright;
          await tester.pumpWidget(
            MaterialApp(
              home: ColoredBox(color: bright ? Colors.blue : Colors.green),
            ),
          );
          final track = peer.remoteParticipants.values
              .expand((person) => person.videoTrackPublications)
              .where(
                (publication) =>
                    publication.source == lk.TrackSource.screenShareVideo,
              )
              .firstOrNull
              ?.track;
          final stats = await track?.getReceiverStats();
          if ((stats?.framesDecoded ?? 0) > 0) {
            expect(stats!.frameWidth, greaterThan(0));
            expect(stats.frameHeight, greaterThan(0));
            break;
          }
          if (DateTime.now().isAfter(deadline)) {
            fail('The remote participant received no decoded screen frames.');
          }
          await Future<void>.delayed(const Duration(milliseconds: 200));
        }
        await media.screenShare(false);
        expect(media.snapshot['screenShareEnabled'], isFalse);
        if (Platform.environment['WADDLE_ISOLATED_CAMERA'] == '1') {
          final sample = img.Image(width: 2, height: 2);
          img.fill(sample, color: img.ColorRgb8(255, 0, 255));
          await media.changeBackground('custom', img.encodePng(sample));
          await media.camera(true);
          await _expectRemoteCamera(peer, 'replacement', (m, _, _) => m > .8);
          await media.changeBackground('blur', null);
          await _expectRemoteCamera(
            peer,
            'blurred',
            (m, l, c) => m < .1 && l > 60 && c < 12,
          );
          await media.changeBackground('off', null);
          await _expectRemoteCamera(
            peer,
            'unprocessed',
            (m, l, c) => m < .1 && l > 60 && c > 18,
          );
          await media.camera(false);
        }
      } finally {
        await media.disconnect();
        if (peer.connectionState != lk.ConnectionState.disconnected) {
          await peer.disconnect();
        }
        await peer.dispose();
        media.dispose();
      }
    },
    skip:
        !Platform.isLinux ||
        path == null ||
        Platform.environment['WADDLE_ISOLATED_CAPTURE'] != '1',
  );
}

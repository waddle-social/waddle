import 'dart:convert';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:livekit_client/livekit_client.dart' as lk;
import 'package:waddle_flutter/runtime/platform_media.dart';

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

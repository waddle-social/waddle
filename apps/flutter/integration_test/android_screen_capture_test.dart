import 'dart:async';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:livekit_client/livekit_client.dart' as lk;
import 'package:path_provider/path_provider.dart';
import 'package:waddle_flutter/runtime/call_screen_share.dart';

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  testWidgets(
    'captures a screen track and receives Android projection revocation',
    (tester) async {
      await tester.pumpWidget(
        const MaterialApp(home: Scaffold(body: Text('Screen capture test'))),
      );
      final sharing = CallScreenShare();
      final room = lk.Room();
      final revoked = Completer<void>();
      lk.LocalVideoTrack? track;
      final ready = File(
        '${(await getTemporaryDirectory()).path}/screen-capture.ready',
      );
      if (await ready.exists()) await ready.delete();
      try {
        track = await sharing.capture(
          room,
          () => true,
          onRevoked: () async => revoked.complete(),
        );
        expect(
          track,
          isNotNull,
          reason: 'Approve the Android screen-sharing prompt.',
        );
        expect(track!.mediaStreamTrack.kind, 'video');
        expect(track.mediaStreamTrack.enabled, isTrue);
        await expectLater(
          const MethodChannel(
            'social.waddle.flutter/camera_effects',
          ).invokeMethod<void>('set', {
            'trackId': track.mediaStreamTrack.id,
            'mode': 'blur',
          }),
          throwsA(
            isA<PlatformException>().having(
              (error) => error.code,
              'code',
              'camera_unavailable',
            ),
          ),
        );
        await ready.writeAsString('active', flush: true);
        await revoked.future.timeout(
          const Duration(seconds: 60),
          onTimeout: () =>
              fail('Stop screen sharing from Android’s system controls.'),
        );
      } finally {
        if (await ready.exists()) await ready.delete();
        await track?.stop();
        await track?.dispose();
        await sharing.release(room);
        await room.dispose();
      }
    },
    skip: !Platform.isAndroid,
  );
}

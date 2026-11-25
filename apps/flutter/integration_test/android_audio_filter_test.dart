// Exercise the pinned LiveKit SDK's native processing diagnostics.
// ignore_for_file: experimental_member_use

import 'dart:io';
import 'dart:math' as math;

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:flutter_webrtc/flutter_webrtc.dart' as rtc;
import 'package:integration_test/integration_test.dart';
import 'package:livekit_client/livekit_client.dart' as lk;

const filters = MethodChannel('social.waddle.flutter/audio_filter');
const signal = bool.fromEnvironment('WADDLE_AUDIO_SIGNAL');

Future<void> setFilter(String id, String mode) =>
    filters.invokeMethod<void>('set', {'trackId': id, 'mode': mode});

Future<rtc.MediaStream> microphone() =>
    rtc.navigator.mediaDevices.getUserMedia({
      'audio': {
        'echoCancellation': false,
        'autoGainControl': false,
        'noiseSuppression': false,
      },
    });

Future<void> describe(
  rtc.RTCPeerConnection sender,
  rtc.RTCPeerConnection receiver,
  rtc.RTCSessionDescription sdp,
) async {
  await sender.setLocalDescription(sdp);
  final elapsed = Stopwatch()..start();
  while (await sender.getIceGatheringState() !=
      rtc.RTCIceGatheringState.RTCIceGatheringStateComplete) {
    expect(elapsed.elapsed, lessThan(const Duration(seconds: 15)));
    await Future<void>.delayed(const Duration(milliseconds: 40));
  }
  await receiver.setRemoteDescription((await sender.getLocalDescription())!);
}

Future<(num, double, double)> received(rtc.RTCPeerConnection peer) async {
  for (final report in await peer.getStats()) {
    final values = report.values;
    if (report.type == 'inbound-rtp' && values['kind'] == 'audio') {
      return (
        values['packetsReceived'] as num,
        (values['totalAudioEnergy'] as num).toDouble(),
        (values['totalSamplesDuration'] as num).toDouble(),
      );
    }
  }
  return (0, 0.0, 0.0);
}

Future<double> measure(rtc.RTCPeerConnection peer, {bool muted = false}) async {
  await Future<void>.delayed(const Duration(milliseconds: 600));
  final start = await received(peer);
  await Future<void>.delayed(const Duration(seconds: 2));
  final end = await received(peer);
  if (muted && start.$1 == end.$1) {
    // WebRTC may suspend the encoder entirely for a disabled track.
    expect(end.$2, closeTo(start.$2, .00001));
    return 0;
  }
  expect(
    end.$1,
    greaterThan(start.$1),
    reason: 'The receiver must decode new RTP audio.',
  );
  expect(end.$3, greaterThan(start.$3));
  return math.sqrt((end.$2 - start.$2) / (end.$3 - start.$3));
}

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  testWidgets(
    'private microphone filters reach a WebRTC receiver and fence other recordings',
    (tester) async {
      if (signal) {
        await rtc.WebRTC.initialize(options: {'bypassVoiceProcessing': true});
      }
      final local = await lk.LocalAudioTrack.create(
        const lk.AudioCaptureOptions(
          echoCancellation: false,
          autoGainControl: false,
          noiseSuppression: false,
          highPassFilter: false,
        ),
      );
      final stream = local.mediaStream;
      final track = local.mediaStreamTrack;
      final id = track.id!;
      final sender = await rtc.createPeerConnection({'iceServers': []});
      final receiver = await rtc.createPeerConnection({'iceServers': []});
      rtc.MediaStream? replacement;
      try {
        final other = await microphone();
        try {
          await expectLater(
            setFilter(id, 'rnnoise'),
            throwsA(isA<PlatformException>()),
          );
        } finally {
          await other.getAudioTracks().single.stop();
          await other.dispose();
        }
        await setFilter(id, 'rnnoise');
        await expectLater(microphone(), throwsA(anything));
        final publication = await sender.addTrack(track, stream);
        await expectLater(receiver.addTrack(track, stream), throwsA(anything));
        await describe(sender, receiver, await sender.createOffer());
        await describe(receiver, sender, await receiver.createAnswer());
        final elapsed = Stopwatch()..start();
        while ((await received(receiver)).$1 == 0) {
          expect(elapsed.elapsed, lessThan(const Duration(seconds: 15)));
          await Future<void>.delayed(const Duration(milliseconds: 100));
        }
        final rnnoise = await measure(receiver);
        await setFilter(id, 'dtln');
        final dtln = await measure(receiver);
        track.enabled = false;
        final muted = await measure(receiver, muted: true);
        track.enabled = true;
        await setFilter('retired-microphone', 'off');
        await expectLater(microphone(), throwsA(anything));
        await setFilter(id, 'off');
        final bypass = await measure(receiver);
        if (signal) {
          // The debug fixture fills the AudioRecord callback with stationary stereo noise.
          debugPrint(
            'Decoded RMS: bypass=$bypass rnnoise=$rnnoise dtln=$dtln muted=$muted',
          );
          expect(bypass, greaterThan(.005));
          expect(rnnoise, lessThan(bypass * .1));
          expect(dtln, lessThan(bypass * .1));
          expect(muted, lessThan(.001));
        }
        if (!signal) {
          for (final value in [true, false]) {
            await local.setAudioProcessingOptions(
              value
                  ? const lk.AudioProcessingOptions.communication()
                  : const lk.AudioProcessingOptions.noProcessing(),
            );
            final state = await lk.AudioManager.instance
                .getAudioProcessingState();
            expect(state?.echoCancellation.requested?.enabled, value);
            expect(state?.noiseSuppression.requested?.enabled, value);
            expect(state?.autoGainControl.requested?.enabled, value);
          }
        }
        await setFilter(id, 'rnnoise');
        await local.stop();
        replacement = await microphone();
        final newTrack = replacement.getAudioTracks().single;
        await expectLater(
          setFilter(newTrack.id!, 'dtln'),
          throwsA(isA<PlatformException>()),
          reason: 'The previous sender still retains its retired native track.',
        );
        await publication.replaceTrack(null);
        await setFilter(newTrack.id!, 'dtln');
        await publication.replaceTrack(newTrack);
        await setFilter(id, 'off');
        await expectLater(microphone(), throwsA(anything));
        await measure(receiver);
      } finally {
        await local.stop();
        await replacement?.getAudioTracks().single.stop();
        await sender.close();
        await receiver.close();
        await sender.dispose();
        await receiver.dispose();
        await local.dispose();
        await replacement?.dispose();
      }
    },
    skip: !Platform.isAndroid,
  );
}

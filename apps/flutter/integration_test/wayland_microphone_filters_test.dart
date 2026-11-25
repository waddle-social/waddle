import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:math';
import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:flutter_webrtc/flutter_webrtc.dart' as rtc;
import 'package:integration_test/integration_test.dart';
import 'package:livekit_client/livekit_client.dart' as lk;
import 'package:waddle_flutter/runtime/call_quality.dart';
import 'package:waddle_flutter/runtime/platform_media.dart';

Future<lk.RemoteTrackPublication<lk.RemoteAudioTrack>> _microphone(
  LiveKitCallMedia media,
  lk.Room peer,
) async {
  final deadline = DateTime.now().add(const Duration(seconds: 15));
  while (DateTime.now().isBefore(deadline)) {
    final streams = (await media.quality())['streams'] as List;
    final ids = streams
        .whereType<Map>()
        .where((s) => s['outgoing'] == true && s['source'] == 'microphone')
        .map((s) => (s['id'] as String).split('/').first)
        .toSet();
    final publication = peer.remoteParticipants.values
        .expand((p) => p.audioTrackPublications)
        .where(
          (p) => p.source == lk.TrackSource.microphone && ids.contains(p.sid),
        )
        .firstOrNull;
    if (publication?.track != null) return publication!;
    await Future<void>.delayed(const Duration(milliseconds: 50));
  }
  throw StateError(
    'The viewer did not subscribe to the replacement microphone.',
  );
}

Future<double> _decodedRms(
  lk.RemoteAudioTrack track,
  String stimulus, {
  String sink = 'waddle_capture_test',
}) async {
  final ffmpeg = Platform.environment['WADDLE_FFMPEG']!;
  final speech = Platform.environment['WADDLE_TEST_SPEECH']!;
  final command = [
    '-nostdin',
    '-loglevel',
    'error',
    '-re',
    if (stimulus == 'speech') ...[
      '-stream_loop',
      '-1',
      '-i',
      speech,
    ] else ...[
      '-f',
      'lavfi',
      '-i',
      'anoisesrc=sample_rate=48000:amplitude=0.28:seed=57,lowpass=f=1200',
    ],
    '-ar',
    '48000',
    '-ac',
    '1',
    '-f',
    'pulse',
    '-device',
    sink,
    'microphone fixture',
  ];
  final player = await Process.start(ffmpeg, command);
  final errors = player.stderr.transform(utf8.decoder).join();
  final done = Completer<double>();
  var frames = 0, samples = 0, energy = 0.0;
  final stop = track.addAudioRenderer(
    onFrame: (frame) {
      // Skip the previous stimulus, jitter buffer, and the model's warmup.
      if (++frames <= 80 || done.isCompleted) return;
      final data = ByteData.sublistView(frame.data);
      for (var i = 0; i + 1 < data.lengthInBytes; i += 2) {
        final x = data.getInt16(i, Endian.little) / 32768;
        energy += x * x;
        samples++;
      }
      if (samples >= 48000) done.complete(sqrt(energy / samples));
    },
  );
  try {
    return await done.future.timeout(const Duration(seconds: 15));
  } finally {
    await stop();
    player.kill();
    await player.exitCode;
    final error = await errors;
    if (error.isNotEmpty) debugPrint(error);
  }
}

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  final file = Platform.environment['WADDLE_LIVEKIT_FIXTURE'];
  testWidgets(
    'isolated microphone models filter decoded peer audio',
    (tester) async {
      final fixture = jsonDecode(await File(file!).readAsString()) as Map;
      if (fixture['cert'] case final String pem) {
        SecurityContext.defaultContext.setTrustedCertificates(pem);
      }
      await tester.pumpWidget(const MaterialApp(home: SizedBox()));
      final media = LiveKitCallMedia();
      final peer = lk.Room();
      try {
        await media.connect(
          Map<String, dynamic>.from(fixture['publisher'] as Map),
          false,
          [],
        );
        final viewer = fixture['viewer'] as Map;
        await peer.connect(viewer['url'] as String, viewer['token'] as String);
        await expectLater(
          rtc.navigator.mediaDevices.getUserMedia({
            'audio': {
              'waddleMicrophoneTrack': 'retired-source',
              'waddleAudioFilter': 'rnnoise',
            },
            'video': false,
          }),
          throwsA(anything),
        );
        await media.microphone(true);
        final sid = (await _microphone(media, peer)).sid;
        for (final key in voiceProcessingLabels.keys) {
          final publication = await _microphone(media, peer);
          expect(publication.sid, sid);
          expect(
            await _decodedRms(publication.track!, 'speech'),
            greaterThan(.002),
          );
          await media.setVoiceProcessing(key, false);
        }
        double? baseline, baselineSpeech;
        for (final mode in [
          MicrophoneFilter.off,
          MicrophoneFilter.rnnoise,
          MicrophoneFilter.dtln,
          MicrophoneFilter.off,
        ]) {
          await media.setMicrophoneFilter(mode);
          expect((await media.quality())['noiseFilter'], mode.name);
          final publication = await _microphone(media, peer);
          expect(publication.sid, sid);
          final track = publication.track!;
          final noise = await _decodedRms(track, 'noise');
          final speech = await _decodedRms(track, 'speech');
          debugPrint(
            'Decoded ${mode.name} microphone RMS: noise=$noise speech=$speech',
          );
          baselineSpeech ??= speech;
          expect(speech, greaterThan(baselineSpeech * .5));
          if (mode == MicrophoneFilter.off) {
            expect(noise, greaterThan(0.012));
            if (baseline != null) {
              expect(noise, closeTo(baseline, baseline * 0.3));
            }
            baseline = noise;
          } else {
            expect(noise, lessThan(baseline! * .45));
          }
        }
        if (Platform.environment['WADDLE_TEST_SECOND_MIC']
            case final String secondMic) {
          await media.setMicrophoneFilter(MicrophoneFilter.rnnoise);
          final second = (await media.devices()).singleWhere(
            (item) =>
                item['kind'] == 'audioinput' &&
                (item['label'] as String).contains(secondMic),
          );
          await media.selectDevice('audioinput', second['id'] as String);
          final changed = await _microphone(media, peer);
          expect(changed.sid, sid);
          final oldInput = await _decodedRms(changed.track!, 'speech');
          final newInput = await _decodedRms(
            changed.track!,
            'speech',
            sink: 'waddle_second_input',
          );
          debugPrint(
            'Switched filtered microphone RMS: former=$oldInput selected=$newInput',
          );
          expect(oldInput, lessThan(.002));
          expect(newInput, greaterThan(.002));
        }
        // Consecutive changes must leave the original call connected even while
        // the SDK's publisher negotiation is still settling.
        for (final key in voiceProcessingLabels.keys) {
          await media.setVoiceProcessing(key, true);
        }
        for (final filter in [
          MicrophoneFilter.dtln,
          MicrophoneFilter.off,
          MicrophoneFilter.rnnoise,
        ]) {
          await media.setMicrophoneFilter(filter);
        }
        final beforeMute = await _microphone(media, peer);
        expect(beforeMute.sid, sid);
        await media.microphone(false);
        final deadline = DateTime.now().add(const Duration(seconds: 10));
        while (!beforeMute.muted && DateTime.now().isBefore(deadline)) {
          await Future<void>.delayed(const Duration(milliseconds: 50));
        }
        expect(beforeMute.muted, isTrue);
        await media.microphone(true);
        final resumed = await _microphone(media, peer);
        expect(resumed.sid, sid);
        expect(
          await _decodedRms(
            resumed.track!,
            'speech',
            sink: Platform.environment.containsKey('WADDLE_TEST_SECOND_MIC')
                ? 'waddle_second_input'
                : 'waddle_capture_test',
          ),
          greaterThan(.002),
        );
        await media.microphone(false);
      } finally {
        await media.disconnect();
        await peer.disconnect();
        await peer.dispose();
        media.dispose();
      }
    },
    skip:
        !Platform.isLinux ||
        file == null ||
        Platform.environment['WADDLE_TEST_SPEECH'] == null,
  );
}

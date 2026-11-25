import 'dart:async';

import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:flutter_webrtc/flutter_webrtc.dart' as rtc;
import 'package:livekit_client/livekit_client.dart' as lk;
import 'package:waddle_flutter/runtime/call_microphone.dart';
import 'package:waddle_flutter/runtime/call_quality.dart';

class _Sender extends Fake implements rtc.RTCRtpSender {
  final replacements = <rtc.MediaStreamTrack?>[];

  @override
  Future<void> replaceTrack(rtc.MediaStreamTrack? track) async {
    // The very first sample on a new RTP source must remain gated.
    if (track != null) expect(track.enabled, isFalse);
    replacements.add(track);
  }

  @override
  Future<List<rtc.StatsReport>> getStats() async => [];
}

class _Transceiver extends Fake implements rtc.RTCRtpTransceiver {
  @override
  final _Sender sender = _Sender();
}

class _Publication extends Fake
    implements lk.LocalTrackPublication<lk.LocalAudioTrack> {
  _Publication(this.track);
  @override
  final String sid = 'mic-sid';
  @override
  final lk.LocalAudioTrack track;
}

class _Local extends Fake implements lk.LocalParticipant {
  final transceiver = _Transceiver();
  _Publication? publication;
  int published = 0;

  @override
  Future<void> removePublishedTrack(String sid, {bool notify = true}) async {
    await publication?.track.stop();
    publication = null;
  }

  @override
  lk.LocalTrackPublication? getTrackPublicationBySource(
    lk.TrackSource source,
  ) => publication;

  @override
  Future<lk.LocalTrackPublication<lk.LocalAudioTrack>> publishAudioTrack(
    lk.LocalAudioTrack track, {
    lk.AudioPublishOptions? publishOptions,
  }) async {
    expect(track.mediaStreamTrack.enabled, isFalse);
    await track.start();
    track.transceiver = transceiver;
    published++;
    return publication = _Publication(track);
  }
}

class _Room extends Fake implements lk.Room {
  @override
  final _Local localParticipant = _Local();
  @override
  final roomOptions = const lk.RoomOptions();
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  test(
    'a mute or PTT release during warmup gates the same publication and its replacement',
    () async {
      final messenger =
          TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
      const channel = MethodChannel('FlutterWebRTC.Method');
      final room = _Room();
      final quality = CallQuality();
      final mic = NativeCallMicrophone(room, quality, MicrophoneFilter.off);
      final closed = <String>{};
      final opened = <String>{};
      Completer<void>? preparing, finish;
      var seq = 0;
      var rejectFilter = false;
      messenger.setMockMethodCallHandler(channel, (call) async {
        final args = call.arguments as Map?;
        switch (call.method) {
          case 'getUserMedia':
            final audio = (args!['constraints'] as Map)['audio'] as Map;
            if (audio.containsKey('waddleMicrophoneTrack')) {
              if (rejectFilter) {
                rejectFilter = false;
                throw PlatformException(code: 'model-unavailable');
              }
              preparing?.complete();
              await finish?.future;
            }
            final id = 'capture-${seq++}';
            opened.add(id);
            return {
              'streamId': id,
              'audioTracks': [
                {
                  'id': id,
                  'kind': 'audio',
                  'label': 'Microphone',
                  'enabled': true,
                },
              ],
            };
          case 'trackDispose':
            closed.add(args!['trackId'] as String);
        }
        return null;
      });
      addTearDown(() async {
        mic.retire();
        await room.localParticipant.publication?.track.dispose();
        messenger.setMockMethodCallHandler(channel, null);
      });

      await mic.enable(true);
      final publication = room.localParticipant.publication!;
      final track = publication.track;
      expect(track.mediaStreamTrack.enabled, isTrue);
      preparing = Completer();
      finish = Completer();
      final change = quality.microphoneChange(
        () => mic.changeFilter(MicrophoneFilter.rnnoise),
      );
      await preparing.future;
      await track.mute(stopOnMute: false);
      finish.complete();
      await change;
      expect(track.mediaStreamTrack.enabled, isFalse);
      expect(mic.canChange, isFalse);
      await track.unmute(stopOnMute: false);
      expect(track.mediaStreamTrack.enabled, isTrue);

      preparing = Completer();
      finish = Completer();
      final pending = quality.microphoneChange(
        () => mic.changeFilter(MicrophoneFilter.dtln),
      );
      await preparing.future;
      mic.gate();
      final release = mic.enable(false);
      finish.complete();
      await pending;
      await release;
      expect(track.muted, isTrue);
      expect(closed, opened);
      await track.unmute(stopOnMute: false);
      expect(track.muted, isTrue);

      preparing = null;
      finish = null;
      await mic.enable(true);
      expect(track.mediaStreamTrack.enabled, isTrue);
      expect(room.localParticipant.publication, same(publication));
      expect(room.localParticipant.published, 1);
      expect(
        room.localParticipant.transceiver.sender.replacements.last,
        same(track.mediaStreamTrack),
      );
      rejectFilter = true;
      await expectLater(
        quality.microphoneChange(
          () => mic.changeFilter(MicrophoneFilter.rnnoise),
        ),
        throwsA(anything),
      );
      expect(mic.filter, MicrophoneFilter.dtln);
      expect(track.mediaStreamTrack.enabled, isTrue);
      expect(room.localParticipant.published, 1);

      await mic.enable(false);
      expect(closed, opened);
      await mic.fullReconnect();
      expect(room.localParticipant.publication, isNull);
      await mic.reconnected();
      expect(closed, opened);
      expect(room.localParticipant.published, 1);
    },
  );
}

import 'package:flutter/foundation.dart';
import 'package:flutter_webrtc/flutter_webrtc.dart' as rtc;
import 'package:livekit_client/livekit_client.dart' as lk;

const voiceProcessingLabels = {
  'echoCancellation': 'Echo cancellation',
  'noiseSuppression': 'Noise suppression',
  'autoGainControl': 'Automatic gain control',
};

/// RTP counters are scoped to a single room and simulcast stream.
class CallQuality {
  final _previous = <String, ({num bytes, Duration time})>{};
  final _clock = Stopwatch()..start();
  Future<void> _pending = Future.value();
  final _voice = <String, bool>{
    for (final key in voiceProcessingLabels.keys) key: true,
  };

  Future<void> microphoneChange(Future<void> Function() action) {
    final next = _pending.then((_) => action());
    _pending = next.catchError((Object _) {});
    return next;
  }

  Future<void> get settled => _pending;

  lk.AudioCaptureOptions captureOptions(lk.AudioCaptureOptions base) =>
      base.copyWith(
        echoCancellation: _voice['echoCancellation'],
        noiseSuppression: _voice['noiseSuppression'],
        autoGainControl: _voice['autoGainControl'],
      );

  Future<void> setProcessing(
    lk.Room room,
    bool Function() current,
    String key,
    bool value,
  ) => microphoneChange(() async {
    if (!current() || !voiceProcessingLabels.containsKey(key)) {
      throw StateError('The microphone setting is unavailable.');
    }
    final track = room.localParticipant
        ?.getTrackPublicationBySource(lk.TrackSource.microphone)
        ?.track;
    if (track is! lk.LocalAudioTrack || track.muted) {
      throw StateError('Turn on the microphone to change its processing.');
    }
    final old = track.currentOptions;
    final next = old.copyWith(
      echoCancellation: key == 'echoCancellation' ? value : null,
      noiseSuppression: key == 'noiseSuppression' ? value : null,
      autoGainControl: key == 'autoGainControl' ? value : null,
    );
    if (!kIsWeb && defaultTargetPlatform == TargetPlatform.linux) {
      // Desktop WebRTC exposes the actual AEC/NS/AGC state when it creates the source.
      // Restart only the microphone on its existing sender and retain its input device.
      try {
        await track.restartTrack(next);
        if (current() && track.mediaStreamTrack.getSettings()[key] != value) {
          throw StateError('The microphone did not apply this setting.');
        }
      } catch (_) {
        if (current()) await track.restartTrack(old);
        rethrow;
      } finally {
        if (!current()) await track.stop();
      }
    } else if (!kIsWeb && defaultTargetPlatform == TargetPlatform.android) {
      // The pinned Android SDK applies these options to the active native sender.
      // ignore: experimental_member_use
      await track.setAudioProcessingOptions(next.processing);
    } else {
      throw UnsupportedError(
        'This device cannot change live microphone processing.',
      );
    }
    if (!current()) throw StateError('The call ended.');
    _voice[key] = value;
  });

  Future<Map<String, dynamic>> sample(lk.Room room) async {
    final now = _clock.elapsed;
    final batches = <Future<List<Map<String, dynamic>>>>[];
    for (final person in <lk.Participant>[
      ?room.localParticipant,
      ...room.remoteParticipants.values,
    ]) {
      final local = identical(person, room.localParticipant);
      for (final publication in person.trackPublications.values) {
        final track = publication.track;
        if (track == null || publication.muted) continue;
        final stats = local
            ? track.sender?.getStats()
            : track.receiver?.getStats();
        if (stats == null) continue;
        batches.add(
          _rows(
            stats,
            local: local,
            who: local
                ? 'You'
                : (person.name.isEmpty ? person.identity : person.name),
            sid: publication.sid,
            source: publication.source.name,
            time: now,
          ),
        );
      }
    }
    final rows = [for (final batch in await Future.wait(batches)) ...batch];
    final ids = rows.map((row) => row['id']).toSet();
    _previous.removeWhere((id, _) => !ids.contains(id));
    final track = room.localParticipant
        ?.getTrackPublicationBySource(lk.TrackSource.microphone)
        ?.track;
    return {
      'streams': rows,
      'voice': Map<String, bool>.from(_voice),
      'canChangeVoice':
          !kIsWeb &&
          [
            TargetPlatform.android,
            TargetPlatform.linux,
          ].contains(defaultTargetPlatform) &&
          track is lk.LocalAudioTrack &&
          !track.muted,
    };
  }

  Future<List<Map<String, dynamic>>> _rows(
    Future<List<rtc.StatsReport>> stats, {
    required bool local,
    required String who,
    required String sid,
    required String source,
    required Duration time,
  }) async {
    try {
      return describe(
        await stats,
        local: local,
        who: who,
        sid: sid,
        source: source,
        time: time,
      );
    } catch (_) {
      // A publication may disappear while the native stats call is in flight.
      return [];
    }
  }

  @visibleForTesting
  List<Map<String, dynamic>> describe(
    List<rtc.StatsReport> stats, {
    required bool local,
    required String who,
    required String sid,
    required String source,
    required Duration time,
  }) {
    final byId = {for (final report in stats) report.id: report.values};
    final rows = <Map<String, dynamic>>[];
    for (final report in stats.where(
      (r) => r.type == (local ? 'outbound-rtp' : 'inbound-rtp'),
    )) {
      final v = report.values;
      final id = '$sid/${report.id}';
      final bytes = v[local ? 'bytesSent' : 'bytesReceived'];
      final previous = _previous[id];
      num? bitrate;
      if (bytes is num) {
        if (previous != null &&
            bytes >= previous.bytes &&
            time > previous.time) {
          bitrate =
              (bytes - previous.bytes) *
              8000 /
              (time - previous.time).inMicroseconds;
        }
        _previous[id] = (bytes: bytes, time: time);
      }
      final feedback = local ? byId[v['remoteId']] : v;
      rows.add({
        'id': id,
        'participant': who,
        'source': source,
        'outgoing': local,
        if (v['rid'] is String) 'layer': v['rid'],
        if (bitrate != null) 'kbps': bitrate.round(),
        if (feedback?['packetsLost'] is num)
          'packetsLost': feedback!['packetsLost'],
        if (byId[v['codecId']]?['mimeType'] case final String codec)
          'codec': codec,
        if (v['frameWidth'] is num && v['frameHeight'] is num)
          'resolution': '${v['frameWidth']} × ${v['frameHeight']}',
        if (v['framesPerSecond'] is num) 'fps': v['framesPerSecond'],
      });
    }
    return rows;
  }
}

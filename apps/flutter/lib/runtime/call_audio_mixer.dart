import 'package:flutter_webrtc/flutter_webrtc.dart' as rtc;
import 'package:livekit_client/livekit_client.dart' as lk;

/// Playback levels belong to subscribed remote audio tracks in this room.
class CallAudioMixer {
  final _levels = <String, double>{};

  Iterable<(lk.RemoteParticipant, lk.RemoteTrackPublication)> _tracks(
    lk.Room room,
  ) sync* {
    for (final person in room.remoteParticipants.values) {
      for (final publication in person.audioTrackPublications) {
        if (publication.track is lk.RemoteAudioTrack) {
          yield (person, publication);
        }
      }
    }
  }

  List<Map<String, dynamic>> snapshot(lk.Room room) => [
    for (final (person, publication) in _tracks(room))
      {
        'sid': publication.sid,
        'name': person.name.isEmpty
            ? person.identity.split('@').first
            : person.name,
        'screen': publication.source == lk.TrackSource.screenShareAudio,
        'gain': _levels[publication.sid] ?? 1.0,
      },
  ];

  Future<void> set(lk.Room room, String sid, double gain) async {
    if (!gain.isFinite || gain < 0 || gain > 2) {
      throw ArgumentError('Choose a volume between 0% and 200%.');
    }
    final pub = _tracks(room).where((row) => row.$2.sid == sid).firstOrNull?.$2;
    final track = pub?.track;
    if (track == null) throw StateError('This audio track has ended.');
    await rtc.Helper.setVolume(gain, track.mediaStreamTrack);
    if (identical(pub?.track, track)) _levels[sid] = gain;
  }

  void remove(String sid) => _levels.remove(sid);
}

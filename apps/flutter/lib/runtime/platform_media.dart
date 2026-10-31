import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:livekit_client/livekit_client.dart' as lk;

import 'context.dart';

abstract class CallMedia extends ChangeNotifier {
  Map<String, dynamic> get snapshot;
  Future<void> connect(
    Map<String, dynamic> join,
    bool video,
    List<Map<String, dynamic>> iceServices,
  );
  Future<void> disconnect();
  Future<void> microphone(bool enabled);
  Future<void> camera(bool enabled);
  Future<void> speaker(bool enabled);
  Future<void> flipCamera();
  Widget buildVideo(BuildContext context, {bool local = false});
}

/// Fast grant validation precedes any media network connection. Tokens stay here.
void validateMediaGrant(Map<String, dynamic> join) {
  final url = Uri.tryParse(join['url']?.toString() ?? '');
  if (url == null ||
      url.scheme != 'wss' ||
      url.host.isEmpty ||
      url.userInfo.isNotEmpty) {
    throw StateError(
      'The call service did not provide a secure media endpoint.',
    );
  }
  try {
    final parts = (join['token'] as String).split('.');
    if (parts.length != 3) {
      throw const FormatException();
    }
    final payload = runtimeMap(
      jsonDecode(utf8.decode(base64Url.decode(base64Url.normalize(parts[1])))),
    );
    final grant = runtimeMap(payload['video']);
    if (grant['roomJoin'] != true ||
        grant['room'] != join['room'] ||
        (payload['sub'] != null && payload['sub'] != join['identity']) ||
        join['identity'] is! String ||
        (join['identity'] as String).isEmpty ||
        (payload['exp'] is num &&
            (payload['exp'] as num).toInt() <=
                DateTime.now().millisecondsSinceEpoch ~/ 1000)) {
      throw const FormatException();
    }
  } catch (_) {
    throw StateError('The call service returned an invalid media grant.');
  }
}

class LiveKitCallMedia extends CallMedia {
  lk.Room? _room;
  lk.EventsListener<lk.RoomEvent>? _listener;
  int _epoch = 0;
  bool _speaker = false;
  String _state = 'idle';
  String? _error;

  @override
  Map<String, dynamic> get snapshot => {
    'mediaState': _state,
    'microphoneEnabled':
        _room?.localParticipant?.isMicrophoneEnabled() ?? false,
    'cameraEnabled': _room?.localParticipant?.isCameraEnabled() ?? false,
    'speakerEnabled': _speaker,
    'participants':
        _room?.remoteParticipants.values
            .map(
              (person) => {
                'jid': person.identity,
                'name': person.name.isEmpty
                    ? person.identity.split('@').first
                    : person.name,
                'speaking': person.isSpeaking,
              },
            )
            .toList() ??
        <Map<String, dynamic>>[],
    if (_error != null) 'error': _error,
  };

  @override
  Future<void> connect(
    Map<String, dynamic> join,
    bool video,
    List<Map<String, dynamic>> iceServices,
  ) async {
    validateMediaGrant(join);
    final stopping = disconnect();
    final epoch = _epoch;
    await stopping;
    if (epoch != _epoch) {
      return;
    }
    final next = lk.Room(
      roomOptions: const lk.RoomOptions(adaptiveStream: true, dynacast: true),
    );
    _room = next;
    _state = 'connecting';
    _error = null;
    notifyListeners();
    next.addListener(_changed);
    _listener = next.createListener()
      ..on<lk.RoomReconnectingEvent>((_) {
        if (_room == next) {
          _state = 'reconnecting';
          notifyListeners();
        }
      })
      ..on<lk.RoomReconnectedEvent>((_) {
        if (_room == next) {
          _state = 'connected';
          notifyListeners();
        }
      })
      ..on<lk.RoomDisconnectedEvent>((_) {
        if (_room == next) {
          _state = 'failed';
          _error = 'The media connection ended.';
          notifyListeners();
        }
      });
    try {
      final ice = iceServices
          .map((entry) {
            final type = entry['service_type'] ?? entry['serviceType'];
            final host = entry['host'];
            if (!{'stun', 'turn', 'stuns', 'turns'}.contains(type) ||
                host is! String ||
                host.isEmpty) {
              return null;
            }
            final transport = entry['transport'];
            final port = entry['port'];
            return lk.RTCIceServer(
              urls: [
                '$type:$host${port == null ? '' : ':$port'}${transport == null ? '' : '?transport=$transport'}',
              ],
              username: entry['username'] as String?,
              credential: entry['password'] as String?,
            );
          })
          .whereType<lk.RTCIceServer>()
          .toList();
      await next.connect(
        join['url'] as String,
        join['token'] as String,
        connectOptions: lk.ConnectOptions(
          rtcConfiguration: lk.RTCConfiguration(
            iceServers: ice.isEmpty ? null : ice,
          ),
        ),
      );
      if (epoch != _epoch || _room != next) {
        await next.disconnect();
        await next.dispose();
        return;
      }
      // Denied or absent capture devices preserve receive-only participation.
      try {
        await _capture(next, true, camera: false);
      } catch (_) {
        /* Receive only. */
      }
      if (epoch != _epoch || _room != next) {
        await next.disconnect();
        return;
      }
      if (video) {
        try {
          await _capture(next, true, camera: true);
        } catch (_) {
          /* Receive only. */
        }
      }
      if (epoch != _epoch || _room != next) {
        await next.disconnect();
        return;
      }
      _state = 'connected';
      notifyListeners();
    } catch (_) {
      if (epoch == _epoch && _room == next) {
        _state = 'failed';
        _error = 'The call media could not connect.';
        notifyListeners();
      }
      rethrow;
    }
  }

  void _changed() => notifyListeners();

  @override
  Future<void> disconnect() async {
    final epoch = ++_epoch;
    final previous = _room;
    _room = null;
    final listener = _listener;
    _listener = null;
    if (previous != null) {
      previous.removeListener(_changed);
    }
    // Stop local devices before waiting for media-server disconnection.
    final tracks =
        previous?.localParticipant?.trackPublications.values
            .map((publication) => publication.track)
            .whereType<lk.LocalTrack>()
            .toList() ??
        <lk.LocalTrack>[];
    await Future.wait(
      tracks.map((track) async {
        try {
          await track.stop();
        } catch (_) {
          /* Continue room cleanup. */
        }
      }),
    );
    await listener?.dispose();
    try {
      await previous?.disconnect();
    } finally {
      await previous?.dispose();
      if (epoch == _epoch) {
        _state = 'idle';
        notifyListeners();
      }
    }
  }

  lk.LocalParticipant _local() =>
      _room?.localParticipant ??
      (throw StateError('The call media is not connected.'));
  Future<void> _capture(
    lk.Room room,
    bool enabled, {
    required bool camera,
  }) async {
    final epoch = _epoch;
    if (_room != room) {
      return;
    }
    final participant =
        room.localParticipant ??
        (throw StateError('The call media is not connected.'));
    final existing = participant.getTrackPublicationBySource(
      camera ? lk.TrackSource.camera : lk.TrackSource.microphone,
    );
    lk.LocalTrack? track = existing?.track;
    var owned = false, published = false;
    try {
      if (!enabled || existing != null) {
        if (camera) {
          await participant.setCameraEnabled(enabled);
        } else {
          await participant.setMicrophoneEnabled(enabled);
        }
      } else {
        // Own newly created tracks so permission/publish failures cannot leave a device running.
        owned = true;
        track = camera
            ? await lk.LocalVideoTrack.createCameraTrack(
                room.roomOptions.defaultCameraCaptureOptions,
              )
            : await lk.LocalAudioTrack.create(
                room.roomOptions.defaultAudioCaptureOptions,
              );
        if (epoch != _epoch || _room != room) {
          return;
        }
        if (track is lk.LocalVideoTrack) {
          await participant.publishVideoTrack(track);
        } else if (track is lk.LocalAudioTrack) {
          await participant.publishAudioTrack(track);
        }
        published = true;
      }
      if (epoch == _epoch && _room == room) {
        notifyListeners();
      }
    } finally {
      if (track != null &&
          (epoch != _epoch || _room != room || (owned && !published))) {
        try {
          await track.stop();
        } finally {
          if (owned) {
            await track.dispose();
          }
        }
      }
    }
  }

  @override
  Future<void> microphone(bool enabled) async {
    final room =
        _room ?? (throw StateError('The call media is not connected.'));
    await _capture(room, enabled, camera: false);
  }

  @override
  Future<void> camera(bool enabled) async {
    final room =
        _room ?? (throw StateError('The call media is not connected.'));
    await _capture(room, enabled, camera: true);
  }

  @override
  Future<void> speaker(bool enabled) async {
    await lk.AudioManager.instance.setSpeakerOutputPreferred(enabled);
    _speaker = enabled;
    notifyListeners();
  }

  @override
  Future<void> flipCamera() async {
    final participant = _local();
    for (final publication in participant.videoTrackPublications) {
      final track = publication.track;
      if (track is lk.LocalVideoTrack &&
          track.currentOptions is lk.CameraCaptureOptions) {
        final options = track.currentOptions as lk.CameraCaptureOptions;
        await track.setCameraPosition(
          options.cameraPosition == lk.CameraPosition.front
              ? lk.CameraPosition.back
              : lk.CameraPosition.front,
        );
        return;
      }
    }
    throw StateError('There is no active camera to switch.');
  }

  lk.VideoTrack? _video(bool local) {
    if (local) {
      for (final publication
          in _room?.localParticipant?.videoTrackPublications ??
              <lk.LocalTrackPublication>[]) {
        if (publication.track is lk.VideoTrack) {
          return publication.track as lk.VideoTrack;
        }
      }
    } else {
      for (final participant
          in _room?.remoteParticipants.values ?? <lk.RemoteParticipant>[]) {
        for (final publication in participant.videoTrackPublications) {
          if (publication.track is lk.VideoTrack && !publication.muted) {
            return publication.track as lk.VideoTrack;
          }
        }
      }
    }
    return null;
  }

  @override
  Widget buildVideo(BuildContext context, {bool local = false}) =>
      AnimatedBuilder(
        animation: this,
        builder: (context, _) {
          final track = _video(local);
          return ColoredBox(
            color: Theme.of(context).colorScheme.surfaceContainerHighest,
            child: track == null
                ? Center(
                    child: Icon(
                      Icons.videocam_off_outlined,
                      color: Theme.of(context).colorScheme.onSurfaceVariant,
                    ),
                  )
                : lk.VideoTrackRenderer(
                    track,
                    fit: lk.VideoViewFit.cover,
                    mirrorMode: local
                        ? lk.VideoViewMirrorMode.mirror
                        : lk.VideoViewMirrorMode.off,
                  ),
          );
        },
      );
}

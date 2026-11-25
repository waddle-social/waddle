import 'dart:convert';
import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:livekit_client/livekit_client.dart' as lk;

import '../features/call_stage.dart';
import 'call_screen_share.dart';
import 'call_camera.dart';
import 'call_audio_mixer.dart';
import 'call_quality.dart';
import 'call_microphone.dart';
import 'call_window.dart';
import 'call_connectivity_stub.dart'
    if (dart.library.io) 'call_connectivity_io.dart';
import 'context.dart';

abstract class CallMedia extends ChangeNotifier {
  Map<String, dynamic> get snapshot;
  Future<void> connect(
    Map<String, dynamic> join,
    bool video,
    List<Map<String, dynamic>> iceServices,
  );
  Future<void> disconnect();
  void gateMicrophone() {}
  Future<void> microphone(bool enabled);
  Future<void> camera(bool enabled);
  Future<void> speaker(bool enabled);
  Future<void> flipCamera();
  Future<void> compactWindow(bool enabled) async {}
  Future<void> setPlaybackVolume(String sid, double gain);
  Future<Map<String, dynamic>> quality() async => const {};
  Future<void> setVoiceProcessing(String key, bool value) =>
      throw UnsupportedError('Microphone processing is unavailable.');
  void prepareMicrophoneFilter(MicrophoneFilter filter) {}
  Future<void> setMicrophoneFilter(MicrophoneFilter filter) =>
      throw UnsupportedError('Microphone filters are unavailable.');
  Future<void> changeBackground(String name, Uint8List? bytes);
  Future<List<Map<String, dynamic>>> devices();
  Future<void> selectDevice(String kind, String id);
  Future<void> screenShare(bool enabled);
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
  late final _window = CallWindow(_changed);
  lk.EventsListener<lk.RoomEvent>? _listener;
  int _epoch = 0;
  final _sharing = CallScreenShare();
  String? _cameraDevice;
  NativeCallCamera? _nativeCamera;
  NativeCallMicrophone? _nativeMicrophone;
  MicrophoneFilter _preferredFilter = MicrophoneFilter.off;
  CallAudioMixer? _mixer;
  CallQuality _quality = CallQuality();
  bool _speaker = false;
  String _state = 'idle';
  String? _error;

  @override
  Map<String, dynamic> get snapshot => {
    'mediaState': _state,
    'compactSupported': _window.available,
    'compact': _window.compact,
    'microphoneEnabled':
        _room?.localParticipant?.isMicrophoneEnabled() ?? false,
    'cameraEnabled': _room?.localParticipant?.isCameraEnabled() ?? false,
    'speakerEnabled': _speaker,
    'speakerSupported': lk.AudioManager.instance.canSwitchSpeakerphone,
    'cameraBackgroundSupported': NativeCallCamera.supported,
    'cameraBackground': _nativeCamera?.background ?? 'off',
    if (_room case final room?)
      'audioTracks': _mixer?.snapshot(room) ?? const [],
    'screenShareSupported': _sharing.supported,
    'screenShareBusy': _sharing.busy,
    'screenShareEnabled':
        _room?.localParticipant?.isScreenShareEnabled() ?? false,
    'remoteCamera':
        _room?.remoteParticipants.values.any(
          (person) => person.isCameraEnabled(),
        ) ??
        false,
    'remoteScreenShare':
        _room?.remoteParticipants.values.any(
          (participant) => participant.isScreenShareEnabled(),
        ) ??
        false,
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
      roomOptions: lk.RoomOptions(
        adaptiveStream: true,
        dynacast: true,
        // Server mute/unmute must retain the processor attached to this track.
        // A local user turning off the camera explicitly unpublishes it.
        defaultAudioCaptureOptions: lk.AudioCaptureOptions(
          stopAudioCaptureOnMute: !NativeCallCamera.supported,
        ),
        defaultCameraCaptureOptions: lk.CameraCaptureOptions(
          stopCameraCaptureOnMute: !NativeCallCamera.supported,
        ),
      ),
    );
    _room = next;
    if (NativeCallCamera.supported) {
      _nativeCamera = NativeCallCamera(next);
      _nativeMicrophone = NativeCallMicrophone(
        next,
        _quality,
        _preferredFilter,
      );
      _mixer = CallAudioMixer();
    }
    _cameraDevice = null;
    _state = 'connecting';
    _error = null;
    notifyListeners();
    next.addListener(_changed);
    _listener = next.createListener()
      ..on<lk.TrackUnsubscribedEvent>((event) {
        if (_room == next) _mixer?.remove(event.publication.sid);
      })
      ..on<lk.ActiveSpeakersChangedEvent>((_) => _changed())
      ..on<lk.LocalTrackUnpublishedEvent>((event) async {
        if (event.publication.source == lk.TrackSource.screenShareVideo &&
            !(next.localParticipant?.isScreenShareEnabled() ?? false)) {
          final id = event.publication.track?.mediaStreamTrack.id;
          if (id != null) await _sharing.release(next, trackId: id);
        }
      })
      ..on<lk.RoomReconnectingEvent>((_) async {
        if (_room == next) {
          _state = 'reconnecting';
          notifyListeners();
          await _nativeMicrophone?.fullReconnect();
        }
      })
      ..on<lk.RoomReconnectedEvent>((_) async {
        if (_room == next) {
          _state = 'connected';
          try {
            await _nativeMicrophone?.reconnected();
          } catch (_) {
            if (_room == next) _error = 'The microphone could not resume.';
          }
          if (_room == next) notifyListeners();
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
      await prepareCallConnectivity();
      if (epoch != _epoch || _room != next) return;
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
      await _window.active(true);
      if (epoch != _epoch || _room != next) return;
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

  @override
  Future<void> compactWindow(bool enabled) => _window.setCompact(enabled);

  void _changed() => notifyListeners();

  @override
  Future<void> disconnect() async {
    final epoch = ++_epoch;
    final previous = _room;
    final restoreWindow = _window.active(false);
    final retiringCamera = _nativeCamera?.retire();
    _nativeCamera = null;
    _nativeMicrophone?.retire();
    _nativeMicrophone = null;
    _mixer = null;
    final retiringQuality = _quality;
    _quality = CallQuality();
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
    await restoreWindow;
    await retiringQuality.settled;
    await retiringCamera;
    await listener?.dispose();
    try {
      try {
        if (previous != null) await _sharing.release(previous);
      } finally {
        await previous?.disconnect();
      }
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
    final native = _nativeCamera;
    final microphone = _nativeMicrophone;
    if (!camera && microphone != null) {
      await microphone.enable(enabled);
      if (epoch == _epoch && _room == room) notifyListeners();
      return;
    }
    if (camera && native != null) {
      await native.setEnabled(enabled);
      if (epoch == _epoch && _room == room) notifyListeners();
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
                room.roomOptions.defaultCameraCaptureOptions.copyWith(
                  deviceId: _cameraDevice,
                ),
              )
            : await lk.LocalAudioTrack.create(
                _quality.captureOptions(
                  room.roomOptions.defaultAudioCaptureOptions,
                ),
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
  Future<Map<String, dynamic>> quality() async {
    final room = _room ?? (throw StateError('The call ended.'));
    final data = await _quality.sample(room);
    final microphone = _nativeMicrophone;
    return {
      ...data,
      if (microphone != null) ...{
        'noiseFilter': microphone.filter.name,
        'canChangeFilter': microphone.canChange,
      },
    };
  }

  @override
  void prepareMicrophoneFilter(MicrophoneFilter filter) =>
      _preferredFilter = filter;

  @override
  Future<void> setMicrophoneFilter(MicrophoneFilter filter) async {
    final mic =
        _nativeMicrophone ??
        (throw StateError('Microphone filters are unavailable.'));
    await _quality.microphoneChange(() async {
      if (!identical(mic, _nativeMicrophone)) {
        throw StateError('The call ended.');
      }
      await mic.changeFilter(filter);
      if (!identical(mic, _nativeMicrophone)) {
        throw StateError('The call ended.');
      }
      _preferredFilter = filter;
    });
  }

  @override
  Future<void> setVoiceProcessing(String key, bool value) async {
    final room = _room ?? (throw StateError('The call ended.'));
    await _quality.setProcessing(
      room,
      () => identical(_room, room),
      key,
      value,
      replaceCapture: _nativeMicrophone?.changeProcessing,
    );
  }

  @override
  void gateMicrophone() => _nativeMicrophone?.gate();

  @override
  Future<void> microphone(bool enabled) async {
    final room =
        _room ?? (throw StateError('The call media is not connected.'));
    if (_nativeMicrophone != null) {
      await _capture(room, enabled, camera: false);
      return;
    }
    await _quality.microphoneChange(() async {
      if (_room != room) throw StateError('The call ended.');
      await _capture(room, enabled, camera: false);
    });
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
    final native = _nativeCamera;
    if (native != null) return native.flip();
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

  @override
  Future<void> changeBackground(String name, Uint8List? bytes) async {
    final camera = _nativeCamera;
    if (camera == null) throw StateError('Camera backgrounds are unavailable.');
    final background = await CameraBackground.from(name, bytes);
    if (!identical(camera, _nativeCamera)) {
      throw StateError('The call has ended.');
    }
    try {
      await camera.changeBackground(background);
    } finally {
      if (identical(camera, _nativeCamera)) notifyListeners();
    }
  }

  @override
  Future<void> setPlaybackVolume(String sid, double gain) async {
    final room = _connectedRoom;
    final mixer =
        _mixer ?? (throw StateError('The audio mixer is unavailable.'));
    await mixer.set(room, sid, gain);
    if (_room == room) notifyListeners();
  }

  lk.Room get _connectedRoom =>
      _room ?? (throw StateError('The call media is not connected.'));

  @override
  Future<void> screenShare(bool enabled) {
    final room = _connectedRoom;
    return _sharing.setEnabled(
      room,
      enabled,
      current: () => identical(_room, room),
      changed: () {
        if (_room != null) notifyListeners();
      },
    );
  }

  bool _selectable(String kind) => switch (kind) {
    'audioinput' => !lk.lkPlatformIsMobile(),
    'audiooutput' => !lk.lkPlatformIs(lk.PlatformType.iOS),
    'videoinput' => true,
    _ => false,
  };

  @override
  Future<List<Map<String, dynamic>>> devices() async {
    final room = _connectedRoom;
    final devices = await lk.Hardware.instance.enumerateDevices();
    if (_room != room) throw StateError('The call has ended.');
    return [
      for (final device in devices)
        if (_selectable(device.kind) && device.deviceId.isNotEmpty)
          {
            'id': device.deviceId,
            'label': device.label,
            'kind': device.kind,
            'selected':
                device.deviceId ==
                switch (device.kind) {
                  'audioinput' =>
                    _nativeMicrophone?.device ??
                        room.selectedAudioInputDeviceId,
                  'audiooutput' => room.selectedAudioOutputDeviceId,
                  _ =>
                    _nativeCamera?.device ??
                        _cameraDevice ??
                        room.selectedVideoInputDeviceId,
                },
          },
    ];
  }

  @override
  Future<void> selectDevice(String kind, String id) async {
    final room = _connectedRoom;
    final devices = await lk.Hardware.instance.enumerateDevices(type: kind);
    final device = devices.where((entry) => entry.deviceId == id).firstOrNull;
    if (device == null || !_selectable(kind)) {
      throw StateError('This device is unavailable.');
    }
    if (_room != room) throw StateError('The call has ended.');
    switch (kind) {
      case 'audioinput':
        await _quality.microphoneChange(() async {
          if (_room != room) throw StateError('The call ended.');
          final mic = _nativeMicrophone;
          if (mic != null) {
            await mic.changeInput(id);
          } else {
            await room.setAudioInputDevice(device);
          }
        });
      case 'audiooutput':
        await room.setAudioOutputDevice(device);
      case 'videoinput':
        // A presentation may be the first published video track. Switch only
        // the camera and keep this choice for the next camera publication.
        final track = room.localParticipant
            ?.getTrackPublicationBySource(lk.TrackSource.camera)
            ?.track;
        final native = _nativeCamera;
        if (native != null) {
          await native.select(id);
        } else if (track is lk.LocalVideoTrack) {
          await track.switchCamera(id);
        }
        if (_room == room) _cameraDevice = id;
    }
  }

  List<CallTile> _tiles(lk.Room room) {
    final tiles = <CallTile>[];
    for (final person in <lk.Participant>[
      ?room.localParticipant,
      ...room.remoteParticipants.values,
    ]) {
      final self = identical(person, room.localParticipant);
      for (final source in [
        lk.TrackSource.camera,
        lk.TrackSource.screenShareVideo,
      ]) {
        final publication = person.getTrackPublicationBySource(source);
        final track = publication?.track;
        final screen = source == lk.TrackSource.screenShareVideo;
        final video = track is lk.VideoTrack && publication?.muted == false
            ? lk.VideoTrackRenderer(
                track,
                key: ObjectKey(track),
                fit: screen ? lk.VideoViewFit.contain : lk.VideoViewFit.cover,
                mirrorMode: self && !screen
                    ? lk.VideoViewMirrorMode.mirror
                    : lk.VideoViewMirrorMode.off,
              )
            : null;
        if (screen && (publication == null || publication.muted)) continue;
        tiles.add(
          CallTile(
            id: '${person.sid}/${source.name}',
            name: self
                ? 'You'
                : person.name.isEmpty
                ? person.identity.split('@').first
                : person.name,
            self: self,
            screen: screen,
            speaking: !screen && person.isSpeaking,
            video: video,
          ),
        );
      }
    }
    return tiles;
  }

  @override
  Widget buildVideo(BuildContext context, {bool local = false}) =>
      AnimatedBuilder(
        animation: this,
        builder: (context, _) {
          final room = _room;
          if (room == null) {
            return const Center(child: Icon(Icons.videocam_off_outlined));
          }
          final tiles = _tiles(room);
          if (_window.compact) {
            final tile =
                tiles
                    .where((t) => t.screen && !t.self && t.video != null)
                    .firstOrNull ??
                tiles.where((t) => !t.self && t.video != null).firstOrNull ??
                tiles.where((t) => !t.screen && t.video != null).firstOrNull;
            return tile?.video ??
                const Center(
                  child: Icon(Icons.call, color: Colors.white, size: 48),
                );
          }
          if (local) {
            return tiles
                    .where((tile) => tile.self && !tile.screen)
                    .firstOrNull
                    ?.video ??
                const SizedBox.shrink();
          }
          final active = room.activeSpeakers
              .where((person) => person != room.localParticipant)
              .firstOrNull;
          return CallStage(
            key: ObjectKey(room),
            tiles: tiles,
            activeSpeaker: active == null ? null : '${active.sid}/camera',
          );
        },
      );
}

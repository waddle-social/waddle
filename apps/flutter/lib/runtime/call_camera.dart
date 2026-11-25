import 'package:flutter/services.dart';
import 'package:flutter_webrtc/flutter_webrtc.dart' as rtc;
import 'package:livekit_client/livekit_client.dart' as lk;

import 'platform_files.dart' show processImage;

class CameraBackground {
  const CameraBackground(this.name, [this.image = const {}]);
  final String name;
  final Map<String, dynamic> image;

  static Future<CameraBackground> from(String name, Uint8List? custom) async {
    if (name == 'off' || name == 'blur') return CameraBackground(name);
    final bytes = switch (name) {
      'mountain' || 'office' || 'abstract' => Uint8List.sublistView(
        await rootBundle.load('assets/backgrounds/$name.png'),
      ),
      'custom' when custom != null => custom,
      _ => throw ArgumentError('Choose a camera background.'),
    };
    return CameraBackground(name, await processImage(bytes, rgba: true));
  }
}

class _CameraProcessor implements lk.TrackProcessor<lk.VideoProcessorOptions> {
  _CameraProcessor(this.background);
  static const channel = MethodChannel('social.waddle.flutter/camera_effects');
  final CameraBackground background;
  String? _track;

  @override
  String get name => 'waddle-camera-background';
  @override
  rtc.MediaStreamTrack? get processedTrack => null; // Native hook processes in place.
  @override
  Future<void> init(lk.VideoProcessorOptions options) async {
    final id = options.track.id;
    if (id == null) throw StateError('The camera has no track.');
    _track = id;
    await channel.invokeMethod<void>('set', {
      'trackId': id,
      'mode': background.name == 'blur' ? 'blur' : 'image',
      ...background.image,
    });
  }

  @override
  Future<void> destroy() async {
    final id = _track;
    _track = null;
    if (id != null) {
      await channel.invokeMethod<void>('set', {'trackId': id, 'mode': 'off'});
    }
  }

  @override
  Future<void> restart(lk.VideoProcessorOptions options) async {
    await destroy();
    await init(options);
  }

  @override
  Future<void> onPublish(lk.Room room) async {}
  @override
  Future<void> onUnpublish() async {}
}

/// Own the native camera publication so device changes also install the
/// selected effect before the sender can see a frame from the new camera.
class NativeCallCamera {
  NativeCallCamera(this.room);
  final lk.Room room;
  static bool get supported =>
      lk.lkPlatformIs(lk.PlatformType.android) ||
      lk.lkPlatformIs(lk.PlatformType.linux);
  CameraBackground _background = const CameraBackground('off');
  String get background => _background.name;
  String? device;
  lk.CameraPosition _position = lk.CameraPosition.front;
  bool _retired = false;
  Future<void> _tail = Future.value();

  Future<void> _queue(Future<void> Function() action) {
    final result = _tail.then((_) async {
      if (_retired) throw StateError('The call has ended.');
      await action();
    });
    _tail = result.catchError((Object _) {});
    return result;
  }

  lk.LocalParticipant get _local =>
      room.localParticipant ?? (throw StateError('The call is not connected.'));

  Future<void> _stop() async {
    final publication = _local.getTrackPublicationBySource(
      lk.TrackSource.camera,
    );
    if (publication != null) await _local.removePublishedTrack(publication.sid);
  }

  Future<void> _start() async {
    if (_retired) return;
    final track = await lk.LocalVideoTrack.createCameraTrack(
      room.roomOptions.defaultCameraCaptureOptions.copyWith(
        cameraPosition: _position,
        deviceId: device,
      ),
    );
    var published = false;
    try {
      if (_retired) return;
      if (background != 'off') {
        await track.setProcessor(_CameraProcessor(_background));
      }
      if (_retired) return;
      await _local.publishVideoTrack(track);
      published = true;
    } finally {
      if (!published || _retired) {
        try {
          await track.stop();
        } finally {
          await track.dispose();
        }
      }
    }
  }

  Future<void> setEnabled(bool enabled) => _queue(() async {
    if (enabled && _local.isCameraEnabled()) return;
    await _stop();
    if (enabled) await _start();
  });

  Future<void> _replace(void Function() update) => _queue(() async {
    final enabled = _local.isCameraEnabled();
    await _stop();
    update();
    if (enabled) await _start();
  });

  Future<void> changeBackground(CameraBackground value) =>
      _replace(() => _background = value);
  Future<void> select(String id) => _replace(() => device = id);
  Future<void> flip() => _replace(() {
    device = null;
    _position = _position == lk.CameraPosition.front
        ? lk.CameraPosition.back
        : lk.CameraPosition.front;
  });

  Future<void> retire() {
    _retired = true;
    _background = const CameraBackground('off');
    return _tail;
  }
}

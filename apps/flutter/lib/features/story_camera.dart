import 'dart:async';
import 'package:flutter/material.dart';
import 'package:flutter_webrtc/flutter_webrtc.dart' as rtc;

import '../bridge.dart';
import '../runtime/platform_files.dart' show maxUploadBytes;
import '../runtime/story_capture_io.dart'
    if (dart.library.js_interop) '../runtime/story_capture_web.dart'
    as platform;
import '../runtime/story_recording.dart';

abstract interface class StoryCamera {
  Widget get preview;
  Future<StoryMedia> photo();
  Future<StoryRecording> record();
  Future<void> close();
}

Future<StoryCamera> openStoryCamera(bool video) async {
  final renderer = rtc.RTCVideoRenderer();
  rtc.MediaStream? stream;
  try {
    await renderer.initialize();
    stream = await rtc.navigator.mediaDevices.getUserMedia({
      'video': {
        'width': {'ideal': 640},
        'height': {'ideal': 480},
        'frameRate': {'ideal': 15},
        'facingMode': 'user',
      },
      'audio': video && platform.requestWebRtcMicrophone,
    });
    if (stream.getVideoTracks().isEmpty) {
      throw StateError('No camera is available.');
    }
    renderer.srcObject = stream;
    return _Camera(renderer, stream);
  } catch (_) {
    try {
      await _release(stream);
    } finally {
      await renderer.dispose();
    }
    rethrow;
  }
}

Future<void> _release(rtc.MediaStream? stream) async {
  if (stream == null) return;
  try {
    await Future.wait(stream.getTracks().map((track) => track.stop()));
  } finally {
    await stream.dispose();
  }
}

class _Camera implements StoryCamera {
  _Camera(this.renderer, this.stream);
  final rtc.RTCVideoRenderer renderer;
  final rtc.MediaStream stream;
  @override
  Widget get preview => rtc.RTCVideoView(renderer, mirror: true);
  @override
  Future<StoryMedia> photo() => platform.takePhoto(stream, renderer);
  @override
  Future<StoryRecording> record() => platform.startVideo(stream, renderer);
  @override
  Future<void> close() async {
    try {
      renderer.srcObject = null;
      await _release(stream);
    } finally {
      await renderer.dispose();
    }
  }
}

class StoryCameraDialog extends StatefulWidget {
  const StoryCameraDialog({
    super.key,
    required this.video,
    this.bridge,
    this.open = openStoryCamera,
  });
  final WaddleBridge? bridge;
  final bool video;
  final Future<StoryCamera> Function(bool) open;
  @override
  State<StoryCameraDialog> createState() => _StoryCameraDialogState();
}

class _StoryCameraDialogState extends State<StoryCameraDialog>
    with WidgetsBindingObserver {
  StoryCamera? _camera;
  StoryRecording? _recording;
  Timer? _limit;
  String? _error;
  bool _working = false, _closing = false;
  Future<void> _operation = Future.value();

  bool get _inCall => [
    'incoming',
    'outgoing',
    'connecting',
    'active',
  ].contains(widget.bridge?.call['state']);
  void _callChanged() {
    if (!_inCall || !mounted || _closing) return;
    final route = ModalRoute.of(context);
    if (route == null || !route.isActive) return;
    _closing = true;
    Navigator.of(context).removeRoute(route);
  }

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    widget.bridge?.addListener(_callChanged);
    if (_inCall) {
      _error = 'The camera is in use by a call. Choose existing media.';
    } else {
      _operation = _open();
    }
  }

  Future<void> _open() async {
    try {
      final camera = await widget.open(widget.video);
      if (_closing || !mounted) {
        await camera.close();
        return;
      }
      setState(() => _camera = camera);
    } catch (_) {
      if (mounted && !_closing) {
        setState(
          () => _error =
              'Camera unavailable. Check device permissions or choose existing media.',
        );
      }
    }
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state == AppLifecycleState.paused ||
        state == AppLifecycleState.hidden ||
        state == AppLifecycleState.detached) {
      _cancel();
    }
  }

  void _cancel() {
    if (!mounted || _closing) return;
    _closing = true;
    Navigator.pop(context);
  }

  @override
  void dispose() {
    _closing = true;
    widget.bridge?.removeListener(_callChanged);
    WidgetsBinding.instance.removeObserver(this);
    _limit?.cancel();
    unawaited(
      _operation
          .whenComplete(() async {
            try {
              await _recording?.cancel();
            } finally {
              await _camera?.close();
            }
          })
          .catchError((Object _) {}),
    );
    super.dispose();
  }

  void _take() {
    if (_working || _closing || _camera == null) return;
    setState(() => _working = true);
    _operation = _capture();
  }

  Future<void> _capture() async {
    try {
      final camera = _camera!;
      if (widget.video && _recording == null) {
        _recording = await camera.record();
        if (!_closing) _limit = Timer(const Duration(seconds: 10), _take);
        return;
      }
      _limit?.cancel();
      final recording = _recording;
      _recording = null;
      final media = recording == null
          ? await camera.photo()
          : await recording.finish();
      if (media.bytes.isEmpty || media.bytes.length > maxUploadBytes) {
        throw StateError('Choose media up to 10 MiB.');
      }
      if (mounted && !_closing) {
        _closing = true;
        Navigator.pop(context, media);
      }
    } catch (error) {
      if (mounted && !_closing) {
        setState(
          () => _error =
              'Could not capture media. Check the camera and microphone or choose existing media.',
        );
      }
    } finally {
      if (mounted && !_closing) setState(() => _working = false);
    }
  }

  @override
  Widget build(BuildContext context) => AlertDialog(
    title: Text(widget.video ? 'Record a story' : 'Take a story photo'),
    content: SizedBox(
      width: 420,
      child: AspectRatio(
        aspectRatio: 4 / 3,
        child: _error != null
            ? Center(child: Text(_error!))
            : _camera?.preview ??
                  const Center(child: CircularProgressIndicator()),
      ),
    ),
    actions: [
      TextButton(onPressed: _cancel, child: const Text('Cancel')),
      if (_error != null)
        TextButton(
          onPressed: () {
            _closing = true;
            Navigator.pop(context, 'pick');
          },
          child: const Text('Choose existing media'),
        ),
      if (_error == null)
        FilledButton(
          onPressed: _camera == null || _working ? null : _take,
          child: Text(
            !widget.video
                ? 'Take photo'
                : _recording == null
                ? 'Record up to 10 seconds'
                : 'Finish recording',
          ),
        ),
    ],
  );
}

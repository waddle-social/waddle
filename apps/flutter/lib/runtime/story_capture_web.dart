import 'dart:async';
import 'dart:js_interop';

import 'package:dart_webrtc/dart_webrtc.dart' as browser;
import 'package:flutter_webrtc/flutter_webrtc.dart' as rtc;
import 'package:web/web.dart' as web;

import 'platform_files.dart' show maxUploadBytes;
import 'story_recording.dart';

bool get requestWebRtcMicrophone => true;

Future<StoryMedia> takePhoto(
  rtc.MediaStream stream,
  rtc.RTCVideoRenderer renderer,
) async {
  final video = web.HTMLVideoElement()
    ..muted = true
    ..playsInline = true
    ..srcObject = (stream as browser.MediaStreamWeb).jsStream;
  final canvas = web.HTMLCanvasElement();
  try {
    await video.play().toDart.timeout(const Duration(seconds: 5));
    if (video.videoWidth == 0) throw StateError('Wait for the camera preview.');
    canvas.width = video.videoWidth;
    canvas.height = video.videoHeight;
    (canvas.getContext('2d') as web.CanvasRenderingContext2D).drawImage(
      video,
      0,
      0,
    );
  } finally {
    video.pause();
    video.srcObject = null;
  }
  final ready = Completer<web.Blob>();
  canvas.toBlob(
    ((web.Blob? blob) {
      if (blob == null) {
        ready.completeError(StateError('Photo capture failed.'));
      } else {
        ready.complete(blob);
      }
    }).toJS,
    'image/jpeg',
  );
  final blob = await ready.future;
  if (blob.size > maxUploadBytes) {
    throw StateError('The captured photo is too large.');
  }
  return StoryMedia(
    (await blob.arrayBuffer().toDart).toDart.asUint8List(),
    'story.jpg',
    'image/jpeg',
  );
}

Future<StoryRecording> startVideo(
  rtc.MediaStream stream,
  rtc.RTCVideoRenderer renderer,
) async {
  final source = (stream as browser.MediaStreamWeb).jsStream;
  final mime = [
    'video/mp4;codecs=avc1.42E01E,mp4a.40.2',
    'video/mp4',
    'video/webm;codecs=vp8,opus',
    'video/webm',
  ].where((mime) => web.MediaRecorder.isTypeSupported(mime)).firstOrNull;
  if (mime == null) {
    throw StateError('Video recording is unavailable in this browser.');
  }
  final recorder = web.MediaRecorder(
    source,
    web.MediaRecorderOptions(
      mimeType: mime,
      videoBitsPerSecond: 1600000,
      audioBitsPerSecond: 96000,
    ),
  );
  return _Recording(recorder, mime)..start();
}

class _Recording implements StoryRecording {
  _Recording(this.recorder, this.mime);
  final web.MediaRecorder recorder;
  final String mime;
  final chunks = <web.Blob>[];
  final done = Completer<void>();
  int size = 0;
  Object? failure;
  void start() {
    recorder.ondataavailable = ((web.BlobEvent event) {
      size += event.data.size;
      if (size > maxUploadBytes) {
        failure = StateError('The recording exceeds the 10 MiB limit.');
        if (recorder.state != 'inactive') recorder.stop();
      } else {
        chunks.add(event.data);
      }
    }).toJS;
    recorder.onerror = ((web.Event _) {
      failure = StateError('Camera recording failed.');
      if (!done.isCompleted) done.complete();
    }).toJS;
    recorder.onstop = ((web.Event _) {
      if (!done.isCompleted) done.complete();
    }).toJS;
    recorder.start(250);
  }

  Future<void> _stop() async {
    if (recorder.state != 'inactive') recorder.stop();
    await done.future.timeout(const Duration(seconds: 5));
  }

  @override
  Future<StoryMedia> finish() async {
    try {
      await _stop();
      if (failure != null) throw failure!;
      if (size == 0) throw StateError('The recording contains no frames.');
      final type = mime.split(';').first;
      final blob = web.Blob(chunks.toJS, web.BlobPropertyBag(type: type));
      return StoryMedia(
        (await blob.arrayBuffer().toDart).toDart.asUint8List(),
        type == 'video/mp4' ? 'story.mp4' : 'story.webm',
        type,
      );
    } finally {
      chunks.clear();
    }
  }

  @override
  Future<void> cancel() async {
    try {
      await _stop();
    } finally {
      chunks.clear();
    }
  }
}

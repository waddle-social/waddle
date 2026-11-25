import 'dart:async';
import 'dart:io';

import 'package:flutter/services.dart';
import 'package:flutter_webrtc/flutter_webrtc.dart' as rtc;
import 'package:path_provider/path_provider.dart';

import 'platform_files.dart' show maxUploadBytes;
import 'story_recording.dart';

// FFmpeg opens the default Linux microphone through PipeWire's Pulse interface.
bool get requestWebRtcMicrophone => !Platform.isLinux;

Future<Directory> _temporary() async =>
    (await getTemporaryDirectory()).createTemp('story-');

Future<Uint8List> _frame(rtc.MediaStream stream, File file) async {
  await rtc.WebRTC.invokeMethod('captureFrame', {
    'trackId': stream.getVideoTracks().first.id,
    'peerConnectionId': 'local',
    'path': file.path,
  });
  if (await file.length() > maxUploadBytes) {
    throw StateError('The captured photo is too large.');
  }
  return file.readAsBytes();
}

Future<StoryMedia> takePhoto(
  rtc.MediaStream stream,
  rtc.RTCVideoRenderer renderer,
) async {
  final temp = await _temporary();
  try {
    final bytes = await _frame(stream, File('${temp.path}/frame'));
    final jpeg = bytes.length > 2 && bytes[0] == 0xff && bytes[1] == 0xd8;
    return StoryMedia(
      bytes,
      jpeg ? 'story.jpg' : 'story.png',
      jpeg ? 'image/jpeg' : 'image/png',
    );
  } finally {
    await temp.delete(recursive: true);
  }
}

Future<StoryRecording> startVideo(
  rtc.MediaStream stream,
  rtc.RTCVideoRenderer renderer,
) async {
  final temp = await _temporary();
  final file = File('${temp.path}/story.mp4');
  try {
    if (Platform.isLinux) return await _linuxRecording(stream, temp, file);
    if (Platform.isAndroid) {
      const channel = MethodChannel('social.waddle.flutter/story_video');
      final id = temp.path.split(Platform.pathSeparator).last;
      await channel.invokeMethod<void>('start', {
        'id': id,
        'trackId': stream.getVideoTracks().first.id,
        'portrait': renderer.videoHeight > renderer.videoWidth,
      });
      return _Recording(
        temp,
        file,
        () => channel.invokeMethod<void>('stop', {'id': id}),
      );
    }
    final recorder = rtc.MediaRecorder(albumName: null);
    await recorder.start(
      file.path,
      videoTrack: stream.getVideoTracks().first,
      audioChannel: rtc.RecorderAudioChannel.INPUT,
    );
    return _Recording(temp, file, () async {
      await recorder.stop();
    });
  } catch (_) {
    await temp.delete(recursive: true);
    rethrow;
  }
}

class _Recording implements StoryRecording {
  _Recording(this.temp, this.output, this.stop);
  final Directory temp;
  final File output;
  final Future<void> Function() stop;
  Future<void>? _closed;

  @override
  Future<StoryMedia> finish() async {
    try {
      await (_closed ??= stop());
      final length = await output.length();
      if (length <= 0 || length > maxUploadBytes) {
        throw StateError(
          'The recording exceeds the 10 MiB limit or contains no frames.',
        );
      }
      return StoryMedia(await output.readAsBytes(), 'story.mp4', 'video/mp4');
    } finally {
      if (await temp.exists()) await temp.delete(recursive: true);
    }
  }

  @override
  Future<void> cancel() async {
    try {
      await (_closed ??= stop());
    } finally {
      if (await temp.exists()) await temp.delete(recursive: true);
    }
  }
}

Future<StoryRecording> _linuxRecording(
  rtc.MediaStream stream,
  Directory temp,
  File output,
) async {
  final bundled = File.fromUri(
    File(Platform.resolvedExecutable).parent.uri.resolve('libexec/ffmpeg'),
  );
  final binary = await bundled.exists()
      ? bundled.path
      : (Platform.environment['WADDLE_FFMPEG'] ?? 'ffmpeg');
  final proc = await Process.start(binary, [
    '-nostdin',
    '-hide_banner',
    '-loglevel',
    'error',
    '-thread_queue_size',
    '64',
    '-use_wallclock_as_timestamps',
    '1',
    '-f',
    'image2pipe',
    '-framerate',
    '12',
    '-vcodec',
    'png',
    '-i',
    'pipe:0',
    '-thread_queue_size',
    '64',
    '-f',
    'pulse',
    '-i',
    'default',
    '-c:v',
    'libx264',
    '-preset',
    'veryfast',
    '-pix_fmt',
    'yuv420p',
    '-b:v',
    '1600k',
    '-c:a',
    'aac',
    '-b:a',
    '96k',
    '-shortest',
    '-t',
    '12',
    '-fs',
    '${maxUploadBytes - 128 * 1024}',
    '-movflags',
    '+faststart',
    output.path,
  ]);
  var running = true;
  Object? failure;
  final finished = proc.exitCode.then((code) {
    running = false;
    return code;
  });
  final stderr = proc.stderr.drain<void>(), stdout = proc.stdout.drain<void>();
  final frame = File('${temp.path}/frame.png');
  Future<void> pump() async {
    try {
      while (running) {
        final timer = Stopwatch()..start();
        final bytes = await _frame(stream, frame);
        if (!running) break;
        proc.stdin.add(bytes);
        await proc.stdin.flush();
        final delay = const Duration(milliseconds: 83) - timer.elapsed;
        if (delay > Duration.zero) await Future<void>.delayed(delay);
      }
    } catch (error) {
      failure = error;
    } finally {
      try {
        await proc.stdin.close();
      } catch (_) {}
    }
  }

  final frames = pump();
  return _Recording(temp, output, () async {
    running = false;
    try {
      await frames.timeout(const Duration(seconds: 5));
      final code = await finished.timeout(const Duration(seconds: 6));
      if (code != 0 || failure != null) {
        throw StateError(
          'Camera recording failed. Check the camera and microphone, or choose an existing video.',
        );
      }
    } finally {
      proc.kill(ProcessSignal.sigkill);
      await finished;
      await Future.wait([stderr, stdout]);
    }
  });
}

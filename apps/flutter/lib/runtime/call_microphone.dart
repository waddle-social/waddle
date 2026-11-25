import 'package:flutter/services.dart';
import 'package:flutter_webrtc/flutter_webrtc.dart' as rtc;
import 'package:livekit_client/livekit_client.dart' as lk;

import 'call_quality.dart';

/// The Android filter runs in the captured track. The Linux filter creates a
/// separate output track whose source holds a sink on the physical microphone.
class _Capture {
  _Capture(this.input, this.output);

  final rtc.MediaStream input;
  final rtc.MediaStream output;
  rtc.MediaStreamTrack get track => output.getAudioTracks().single;

  static Future<_Capture> open(
    lk.AudioCaptureOptions options,
    MicrophoneFilter filter, {
    String? setting,
    bool? value,
  }) async {
    final input = await rtc.navigator.mediaDevices.getUserMedia({
      'audio': options.toMediaConstraintsMap(),
      'video': false,
    });
    var output = input;
    try {
      final mic = input.getAudioTracks().single;
      if (setting != null && mic.getSettings()[setting] != value) {
        throw StateError('The microphone did not apply this setting.');
      }
      if (filter != MicrophoneFilter.off) {
        final id = mic.id;
        if (id == null) throw StateError('The microphone has no track.');
        if (lk.lkPlatformIs(lk.PlatformType.android)) {
          const channel = MethodChannel('social.waddle.flutter/audio_filter');
          await channel.invokeMethod<void>('set', {
            'trackId': id,
            'mode': filter.name,
          });
        } else {
          output = await rtc.navigator.mediaDevices.getUserMedia({
            'audio': {
              'waddleMicrophoneTrack': id,
              'waddleAudioFilter': filter.name,
            },
            'video': false,
          });
        }
      }
      final capture = _Capture(input, output);
      capture.track.enabled = false;
      return capture;
    } catch (_) {
      await _Capture(input, output).close();
      rethrow;
    }
  }

  static Future<void> _closeStream(rtc.MediaStream stream) async {
    try {
      for (final track in stream.getAudioTracks()) {
        await track.stop();
      }
    } finally {
      await stream.dispose();
    }
  }

  Future<void> closeInput() async {
    if (!identical(input, output)) await _closeStream(input);
  }

  Future<void> close() async {
    try {
      await _closeStream(output);
    } finally {
      await closeInput();
    }
  }
}

/// The SFU publication, sender, mute events and statistics all refer to this
/// object throughout the call. The device and model underneath it may change.
class _PublishedMicrophone extends lk.LocalAudioTrack {
  _PublishedMicrophone(_Capture initial, lk.AudioCaptureOptions options)
    : _capture = initial,
      // LiveKit's factory opens its own device. This wrapper owns an already
      // prepared native output, so it needs the stream-taking constructor.
      // ignore: invalid_use_of_internal_member
      super(lk.TrackSource.microphone, initial.output, initial.track, options) {
    _onEnded = initial.track.onEnded;
  }

  late final Function()? _onEnded;

  _Capture? _capture;
  bool localEnabled = true;
  bool remoteMuted = false;
  bool _ready = false;

  void _applyMute() {
    final quiet = !localEnabled || remoteMuted;
    final output = _capture?.track;
    final enabled = _ready && !quiet;
    if (output != null && output.enabled != enabled) output.enabled = enabled;
    if (muted != quiet) {
      // LiveKit has a single publication mute bit. Report the combined local
      // and moderator state without starting its unfiltered track restart.
      // ignore: invalid_use_of_internal_member
      updateMuted(quiet, shouldSendSignal: true);
    }
  }

  void requestLocal(bool enabled) {
    localEnabled = enabled;
    _applyMute();
  }

  void ready() {
    _ready = true;
    _applyMute();
  }

  @override
  Future<bool> mute({bool stopOnMute = true}) async {
    final previous = muted;
    remoteMuted = true;
    _applyMute();
    return muted != previous;
  }

  @override
  Future<bool> unmute({bool stopOnMute = true}) async {
    final previous = muted;
    remoteMuted = false;
    _applyMute();
    return muted != previous;
  }

  Future<void> adopt(_Capture capture, lk.AudioCaptureOptions options) async {
    // LiveKit's restartTrack attaches the raw capture before processor setup.
    // Update its existing wrapper only after preparing the replacement output.
    // ignore: invalid_use_of_internal_member
    updateMediaStreamAndTrack(capture.output, capture.track);
    capture.track.onEnded = _onEnded;
    currentOptions = options;
    _capture = capture;
    await start();
  }

  @override
  Future<bool> stop() async {
    final capture = _capture;
    if (capture == null) return false;
    _ready = false;
    _capture = null;
    capture.track.enabled = false;
    capture.track.onEnded = null;
    try {
      // LocalAudioTrack stops the output track and disposes its stream.
      return await super.stop();
    } finally {
      await capture.closeInput();
    }
  }
}

/// Serializes microphone changes within one room and prepares each filter
/// before the RTP sender can receive audio from the replacement capture.
class NativeCallMicrophone {
  NativeCallMicrophone(this.room, this.quality, this.filter);
  final lk.Room room;
  final CallQuality quality;
  MicrophoneFilter filter;
  String? device;

  _PublishedMicrophone? _track;
  bool _wanted = false;
  bool _retired = false;
  bool _reconnecting = false;
  bool _restoreCapture = false;
  int _revision = 0;

  void gate() {
    _wanted = false;
    ++_revision;
    _track?.requestLocal(false);
  }

  void retire() {
    _retired = true;
    gate();
  }

  lk.LocalParticipant get _local =>
      room.localParticipant ?? (throw StateError('The call is not connected.'));

  lk.AudioCaptureOptions get _options => quality
      .captureOptions(room.roomOptions.defaultAudioCaptureOptions)
      .copyWith(deviceId: device);

  bool get canChange => _track?.remoteMuted != true;

  bool _current(int revision) =>
      !_retired && !_reconnecting && _wanted && _revision == revision;

  Future<void> fullReconnect() async {
    _reconnecting = true;
    ++_revision;
    final track = _track;
    final publication = _local.getTrackPublicationBySource(
      lk.TrackSource.microphone,
    );
    if (track == null ||
        publication == null ||
        !identical(publication.track, track)) {
      return;
    }
    if (_wanted && track._ready && track._capture != null) return;
    // A full peer restart republishes every LiveKit publication. A sender with
    // its physical device released has no live RTC track to attach to a new peer.
    _restoreCapture = _wanted && !track.remoteMuted;
    _track = null;
    await _local.removePublishedTrack(publication.sid);
  }

  Future<void> reconnected() async {
    if (!_reconnecting) return;
    _reconnecting = false;
    final restore = _restoreCapture;
    _restoreCapture = false;
    if (!_retired && _wanted && restore) await enable(true);
  }

  bool _published(_PublishedMicrophone track) => identical(
    _local.getTrackPublicationBySource(lk.TrackSource.microphone)?.track,
    track,
  );

  Future<void> _detach(_PublishedMicrophone track) async {
    try {
      await track.sender?.replaceTrack(null);
    } finally {
      await track.stop();
    }
  }

  Future<void> _attach(
    int revision,
    lk.AudioCaptureOptions options,
    MicrophoneFilter model, {
    String? setting,
    bool? value,
  }) async {
    final prior = _track;
    final reuse = prior != null && _published(prior);
    final capture = await _Capture.open(
      options,
      model,
      setting: setting,
      value: value,
    );
    if (!_current(revision)) {
      await capture.close();
      return;
    }
    if (reuse && !_published(prior)) {
      await capture.close();
      throw StateError('The microphone publication ended.');
    }
    final track = reuse ? prior : _PublishedMicrophone(capture, options);
    _track = track;
    var ownedByTrack = !reuse;
    try {
      if (reuse) {
        final sender = track.sender;
        if (sender == null) {
          throw StateError('The microphone sender is unavailable.');
        }
        ownedByTrack = true;
        await track.adopt(capture, options);
        if (!_current(revision) || !_published(track)) {
          await track.stop();
          return;
        }
        await sender.replaceTrack(capture.track);
      } else {
        await _local.publishAudioTrack(track);
      }
      if (!_current(revision) || !_published(track)) {
        await _detach(track);
        return;
      }
      track.requestLocal(true);
      track.ready();
    } catch (_) {
      if (!ownedByTrack) {
        await capture.close();
      } else {
        await track.stop();
      }
      if (!reuse) _track = null;
      rethrow;
    }
  }

  Future<void> enable(bool enabled) {
    if (_retired) return Future.value();
    if (_wanted != enabled) ++_revision;
    _wanted = enabled;
    final revision = _revision;
    if (_reconnecting) {
      _restoreCapture = enabled;
      if (!enabled) {
        _track?.requestLocal(false);
        return fullReconnect();
      }
      return Future.value();
    }
    // A PTT release silences the active output immediately, even during a
    // pending capture open or before an earlier change finishes rolling back.
    if (!enabled) _track?.requestLocal(false);
    return quality.microphoneChange(() async {
      if (_retired) return;
      final track = _track;
      if (!enabled) {
        if (track != null && _published(track)) await _detach(track);
      } else if (_current(revision)) {
        if (track != null && track._capture != null && _published(track)) {
          track.requestLocal(true);
        } else {
          await _attach(revision, _options, filter);
        }
      }
    });
  }

  Future<void> _replace(
    lk.AudioCaptureOptions options,
    MicrophoneFilter model, {
    String? setting,
    bool? value,
  }) async {
    if (_reconnecting) throw StateError('The call is reconnecting.');
    if (!canChange) throw StateError('The published microphone is muted.');
    final track = _track;
    if (!_wanted || track == null || !_published(track)) return;
    final revision = _revision;
    final previous = track.currentOptions;
    try {
      await _detach(track);
      if (!_current(revision)) return;
      if (!_published(track)) {
        throw StateError('The microphone publication ended.');
      }
      await _attach(revision, options, model, setting: setting, value: value);
    } catch (_) {
      if (_current(revision) && _published(track)) {
        try {
          await _attach(revision, previous, filter);
        } catch (_) {
          _wanted = false;
          track.requestLocal(false);
          rethrow;
        }
      }
      rethrow;
    }
  }

  Future<void> changeFilter(MicrophoneFilter model) async {
    if (model == filter) return;
    await _replace(_options, model);
    filter = model;
  }

  Future<void> changeInput(String id) async {
    await _replace(_options.copyWith(deviceId: id), filter);
    device = id;
  }

  Future<void> changeProcessing(
    lk.AudioCaptureOptions next,
    String key,
    bool value,
  ) => _replace(next, filter, setting: key, value: value);
}

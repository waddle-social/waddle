import 'dart:async';

import 'package:flutter/services.dart';
import 'package:flutter_webrtc/flutter_webrtc.dart' as rtc;
import 'package:livekit_client/livekit_client.dart' as lk;

/// A projection service belongs to the room that received Android's consent.
class CallScreenShare {
  static const _service = MethodChannel('social.waddle.flutter/screen_capture');
  lk.Room? _serviceOwner;
  String? _serviceTrackId;
  lk.Room? _waitingOwner;
  Completer<bool>? _waitingForFrame;
  Future<void> _stopping = Future<void>.value();
  bool busy = false;

  bool get supported =>
      lk.lkPlatformIs(lk.PlatformType.android) ||
      lk.lkPlatformIs(lk.PlatformType.linux) ||
      (lk.lkPlatformIs(lk.PlatformType.web) && !lk.lkPlatformIsWebMobile());

  Future<void> release(lk.Room room, {String? trackId}) async {
    if (identical(_waitingOwner, room) &&
        _waitingForFrame?.isCompleted == false) {
      _waitingForFrame!.complete(false);
    }
    if (!identical(_serviceOwner, room) ||
        (trackId != null && trackId != _serviceTrackId)) {
      return;
    }
    _serviceTrackId = null;
    _serviceOwner = null;
    _service.setMethodCallHandler(null);
    final stopping = _service.invokeMethod<void>('stop');
    _stopping = stopping;
    try {
      await stopping;
    } finally {
      if (identical(_stopping, stopping)) _stopping = Future<void>.value();
    }
  }

  Future<lk.LocalVideoTrack?> capture(
    lk.Room room,
    bool Function() current, {
    Future<void> Function()? onRevoked,
  }) async {
    if (lk.lkPlatformIs(lk.PlatformType.android)) {
      // The pinned SDK exposes Android's pre-service consent through this API.
      // ignore: experimental_member_use
      if (!await lk.Hardware.instance.requestCapturePermission() ||
          !current()) {
        return null;
      }
      await _stopping;
      if (!current()) return null;
      await _service.invokeMethod<void>('start');
      _serviceOwner = room;
      if (!current()) return null;
      String? trackId;
      final earlyStops = <String>{};
      _service.setMethodCallHandler((call) async {
        if (call.method != 'revoked' || call.arguments is! String) return;
        if (trackId == null) {
          earlyStops.add(call.arguments as String);
        } else if (call.arguments == trackId) {
          await onRevoked?.call();
        }
      });
      final track = await lk.LocalVideoTrack.createScreenShareTrack();
      trackId = track.mediaStreamTrack.id;
      if (identical(_serviceOwner, room)) _serviceTrackId = trackId;
      if (earlyStops.contains(trackId)) await onRevoked?.call();
      return track;
    }
    final track = await lk.LocalVideoTrack.createScreenShareTrack();
    if (!lk.lkPlatformIs(lk.PlatformType.linux)) return track;

    // On Wayland getDisplayMedia returns before the portal grants capture.
    // Keep the unpublished track local until a frame actually arrives.
    final firstFrame = Completer<bool>();
    final renderer = rtc.RTCVideoRenderer();
    _waitingForFrame = firstFrame;
    _waitingOwner = room;
    var ready = false;
    try {
      if (!current()) return null;
      await renderer.initialize();
      renderer.onFirstFrameRendered = () {
        if (!firstFrame.isCompleted) firstFrame.complete(true);
      };
      renderer.srcObject = track.mediaStream;
      ready = await firstFrame.future.timeout(
        const Duration(minutes: 1),
        onTimeout: () =>
            throw StateError('The desktop did not grant screen capture.'),
      );
      return ready ? track : null;
    } finally {
      _waitingForFrame = null;
      _waitingOwner = null;
      try {
        await renderer.dispose();
      } finally {
        if (!ready) {
          try {
            await track.stop();
          } finally {
            await track.dispose();
          }
        }
      }
    }
  }

  Future<void> setEnabled(
    lk.Room room,
    bool enabled, {
    required bool Function() current,
    required void Function() changed,
  }) async {
    if (!supported || busy) {
      throw StateError('Screen sharing is unavailable right now.');
    }
    final participant = room.localParticipant;
    if (participant == null) throw StateError('The call has ended.');
    busy = true;
    changed();
    lk.LocalVideoTrack? track;
    String? publishedSid;
    var revoked = false;
    Future<void> removeRevokedPublication() async {
      revoked = true;
      final sid = publishedSid;
      if (sid == null) return;
      try {
        await participant.removePublishedTrack(sid);
      } finally {
        await release(room, trackId: track?.mediaStreamTrack.id);
        changed();
      }
    }

    try {
      if (!enabled) {
        await participant.setScreenShareEnabled(false);
        return;
      }
      if (participant.isScreenShareEnabled()) return;
      track = await capture(room, current, onRevoked: removeRevokedPublication);
      if (track == null || !current() || revoked) return;
      publishedSid = (await participant.publishVideoTrack(track)).sid;
      if (revoked) await removeRevokedPublication();
    } finally {
      try {
        if (track != null && (publishedSid == null || !current())) {
          try {
            await track.stop();
          } finally {
            await track.dispose();
          }
        }
      } finally {
        try {
          if (!current() || !participant.isScreenShareEnabled()) {
            await release(room);
          }
        } finally {
          busy = false;
          changed();
        }
      }
    }
  }
}

import 'dart:async';

import 'package:flutter/services.dart';
import 'package:flutter_webrtc/flutter_webrtc.dart' as rtc;
import 'package:livekit_client/livekit_client.dart' as lk;

/// The room owns its capture track, platform consent and teardown.
class CallScreenShare {
  static const _service = MethodChannel('social.waddle.flutter/screen_capture');
  static int _nextToken = 0;
  lk.Room? _serviceOwner;
  String? _serviceTrackId;
  lk.Room? _waitingOwner;
  Completer<bool>? _waitingForGrant;
  Future<void> _stopping = Future<void>.value();
  bool busy = false;

  bool get supported =>
      lk.lkPlatformIs(lk.PlatformType.android) ||
      lk.lkPlatformIs(lk.PlatformType.linux) ||
      (lk.lkPlatformIs(lk.PlatformType.web) && !lk.lkPlatformIsWebMobile());

  Future<void> release(lk.Room room, {String? trackId}) async {
    if (!identical(_serviceOwner, room) ||
        (trackId != null && trackId != _serviceTrackId)) {
      return;
    }
    if (identical(_waitingOwner, room) &&
        _waitingForGrant?.isCompleted == false) {
      _waitingForGrant!.complete(false);
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
    if (!lk.lkPlatformIs(lk.PlatformType.linux)) {
      return lk.LocalVideoTrack.createScreenShareTrack();
    }

    // WebRTC returns a track before the Wayland picker grants capture.
    // The portal confirms the selected streams even when a static screen
    // has no new frame for an offscreen Flutter texture.
    await _stopping;
    if (!current()) return null;
    final grant = Completer<bool>();
    final token = ++_nextToken;
    _waitingForGrant = grant;
    _waitingOwner = room;
    _serviceOwner = room;
    _serviceTrackId = null;
    _service.setMethodCallHandler((call) async {
      if (call.arguments != token) return;
      if (call.method == 'granted') {
        if (!grant.isCompleted) grant.complete(true);
      } else if (call.method == 'revoked') {
        if (!grant.isCompleted) grant.complete(false);
        await onRevoked?.call();
      }
    });
    lk.LocalVideoTrack? track;
    var ready = false;
    try {
      final enumerate = await _service.invokeMethod<bool>('prepare', token);
      if (enumerate == true) {
        await rtc.desktopCapturer.getSources(types: [rtc.SourceType.Screen]);
        final prepared = await _service
            .invokeMethod<bool>('capture')
            .timeout(const Duration(seconds: 10), onTimeout: () => false);
        if (prepared != true) {
          throw StateError('The desktop capture portal is unavailable.');
        }
      }
      if (!current() || grant.isCompleted) return null;
      track = await lk.LocalVideoTrack.createScreenShareTrack();
      _serviceTrackId = track.mediaStreamTrack.id;
      if (!current()) return null;
      ready =
          enumerate == null ||
          await grant.future.timeout(
            const Duration(minutes: 1),
            onTimeout: () =>
                throw StateError('The desktop did not grant screen capture.'),
          );
      return ready ? track : null;
    } finally {
      _waitingForGrant = null;
      _waitingOwner = null;
      if (!ready) {
        try {
          await track?.stop();
        } finally {
          await track?.dispose();
          await release(room);
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

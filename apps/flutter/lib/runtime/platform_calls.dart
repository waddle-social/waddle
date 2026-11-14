import 'dart:async';
import 'dart:math';

import 'package:flutter/foundation.dart';
import 'package:flutter/widgets.dart';
import 'package:permission_handler/permission_handler.dart';

import 'context.dart';
import 'platform_media.dart';

Future<void> requestCallCapture(bool video) async {
  // Unsupported desktop permission APIs do not prohibit receive-only participation.
  if (!kIsWeb &&
      !{
        TargetPlatform.android,
        TargetPlatform.iOS,
        TargetPlatform.macOS,
      }.contains(defaultTargetPlatform)) {
    return;
  }
  try {
    await [Permission.microphone, if (video) Permission.camera].request();
  } catch (_) {
    /* Actual publication is decided by the media SDK/device grants. */
  }
}

String _fullKey(String value) {
  final slash = value.indexOf('/');
  return slash < 0
      ? value.toLowerCase()
      : '${value.substring(0, slash).toLowerCase()}${value.substring(slash)}';
}

class _ForegroundOwner {
  _ForegroundOwner(this.sid);
  final String sid;
}

class PlatformCallService {
  PlatformCallService(
    this.context, {
    CallMedia? media,
    Future<void> Function(bool)? requestCapture,
    Future<void> Function(bool active, bool video, String sid)? callForeground,
    this.setupTimeout = const Duration(seconds: 30),
  }) : media = media ?? LiveKitCallMedia(),
       _requestCapture = requestCapture ?? requestCallCapture,
       _callForeground = callForeground ?? ((_, _, _) async {}) {
    this.media.addListener(_mediaChanged);
  }
  final RuntimeContext context;
  final CallMedia media;
  final Future<void> Function(bool) _requestCapture;
  final Future<void> Function(bool active, bool video, String sid)
  _callForeground;
  final Duration setupTimeout;
  final _events = StreamController<Map<String, dynamic>>.broadcast(sync: true);
  final _presenceChanges = StreamController<void>.broadcast(sync: true);
  final _rooms = <String, Map<String, Map<String, dynamic>>>{};
  String _phase = 'idle', _sid = '', _peer = '';
  bool _group = false, _video = false, _accepting = false, _hand = false;
  bool _activePresence = false, _disposed = false;
  int _epoch = 0;
  int _nextSignal = 0;
  final _signaling = <int>{};
  final _retainedLeaves = <String, Object>{};
  String? _error;
  DateTime? _started;
  Timer? _timeout, _duration;
  Completer<Map<String, dynamic>>? _mixerAccept;
  Future<void> _presenceTail = Future.value();
  Future<void> _foregroundTail = Future.value();
  _ForegroundOwner? _foregroundOwner;
  final _foregroundTeardowns = <_ForegroundOwner>{};

  Stream<Map<String, dynamic>> get events => _events.stream;
  Widget buildVideo(BuildContext context, {bool local = false}) =>
      media.buildVideo(context, local: local);
  String get _ownFull =>
      context.session['fullJid']?.toString() ??
      (throw StateError('The XMPP resource is not ready.'));
  String get _nick =>
      (context.session['xmppLocalpart'] ??
              context.session['xmpp_localpart'] ??
              context.jid?.split('@').first)
          ?.toString() ??
      '';
  Map<String, dynamic> get _cache =>
      runtimeMap(context.persistent['callSessions']);

  bool _current(int epoch, int generation, [String? sid]) =>
      !_disposed &&
      context.generation == generation &&
      _epoch == epoch &&
      (sid == null || sid == _sid);
  void _guard(int epoch, int generation, [String? sid]) {
    requireGeneration(context, generation);
    if (!_current(epoch, generation, sid)) {
      throw StateError('The call changed during this operation.');
    }
  }

  void _publish() {
    if (_disposed || _events.isClosed) {
      return;
    }
    final mediaView = Map<String, dynamic>.from(media.snapshot);
    if (!{'active', 'ended'}.contains(_phase)) {
      mediaView.remove('error');
    }
    if (_group) {
      final people = runtimeRows(mediaView['participants']);
      for (final person in people) {
        final jid = bareJid(person['jid']?.toString() ?? '');
        final occupant = (_rooms[_peer] ?? {}).values
            .where(
              (presence) =>
                  bareJid(presence['muc_jid']?.toString() ?? '') == jid,
            )
            .firstOrNull;
        if (occupant != null) {
          person['handRaised'] = occupant['hand_raised'] == true;
          person['muted'] = occupant['muted'] == true;
        }
      }
      mediaView['participants'] = people;
    }
    _events.add({
      'topic': 'call',
      'generation': context.generation,
      'data': {
        ...mediaView,
        'state': _phase,
        'signaling': _signaling.isNotEmpty,
        'sid': _sid.isEmpty ? null : _sid,
        'peerJid': _peer,
        'video': _video,
        'group': _group,
        'accepting': _accepting,
        'handRaised': _hand,
        'durationSeconds': _started == null
            ? 0
            : DateTime.now().difference(_started!).inSeconds,
        if (_error != null) 'error': _error,
      },
    });
  }

  Future<dynamic> invoke(String method, Map<String, dynamic> args) async {
    if (method.startsWith('call.') &&
        method != 'call.start' &&
        method != 'call.roomState' &&
        args.containsKey('expectedSid') &&
        (args['expectedSid']?.toString() ?? '') != _sid) {
      throw StateError('The call changed before this action was applied.');
    }
    switch (method) {
      case 'call.start':
        return _start(args);
      case 'call.accept':
        return _accept(
          args['expectedSid']?.toString() ?? args['sid']?.toString() ?? _sid,
        );
      case 'call.decline':
        return _decline(
          args['expectedSid']?.toString() ?? args['sid']?.toString() ?? _sid,
        );
      case 'call.hangup':
        return _hangUp();
      case 'call.dismiss':
        if (_phase != 'ended' && _phase != 'idle') {
          throw StateError('End the current call first.');
        }
        _phase = 'idle';
        _sid = '';
        _peer = '';
        _error = null;
        _publish();
        return null;
      case 'call.microphone':
      case 'call.camera':
        final epoch = _epoch, generation = context.generation, sid = _sid;
        if (_phase != 'active') {
          throw StateError('The call is not active.');
        }
        final camera = method == 'call.camera';
        final enabled =
            args['enabled'] as bool? ??
            media.snapshot[camera ? 'cameraEnabled' : 'microphoneEnabled'] !=
                true;
        if (enabled) {
          await _requestCapture(camera);
        }
        _guard(epoch, generation, sid);
        if (enabled) {
          await _startForeground(
            epoch,
            generation,
            sid,
            video: camera || _video,
            refresh: true,
          );
          _guard(epoch, generation, sid);
        }
        if (camera) {
          await media.camera(enabled);
        } else {
          await media.microphone(enabled);
        }
        _guard(epoch, generation, sid);
        if (_group && !camera) {
          await _sendPresence(sid, epoch, generation, active: true);
        }
        _publish();
        return null;
      case 'call.speaker':
        final speakerEpoch = _epoch, speakerGeneration = context.generation;
        await media.speaker(
          args['enabled'] as bool? ?? media.snapshot['speakerEnabled'] != true,
        );
        _guard(speakerEpoch, speakerGeneration);
        _publish();
        return null;
      case 'call.flipCamera':
        final cameraEpoch = _epoch, cameraGeneration = context.generation;
        await media.flipCamera();
        _guard(cameraEpoch, cameraGeneration);
        return null;
      case 'call.hand':
        if (!_group || _phase != 'active') {
          throw StateError('Raise your hand in an active room call.');
        }
        _hand = args['raised'] as bool? ?? !_hand;
        await _sendPresence(_sid, _epoch, context.generation, active: true);
        _publish();
        return null;
      case 'call.roomState':
        return _roomState(args['jid'] as String);
      case 'call.leaveRetained':
        return _leaveRetained(args['jid'] as String);
      default:
        throw UnsupportedError('Unsupported call operation: $method');
    }
  }

  Future<dynamic> _rpc(String method, Map<String, dynamic> args) async {
    final result = await context.rpc(method, args);
    if (result == false || result == 'error') {
      throw StateError('The call service refused this operation.');
    }
    return result;
  }

  Future<void> _start(Map<String, dynamic> args) async {
    if (_phase != 'idle' && _phase != 'ended') {
      throw StateError('Another call is already in progress.');
    }
    final jid = args['jid']?.toString() ?? '';
    if (!jid.contains('@') || bareJid(jid) == context.jid) {
      throw StateError('Choose another person or a room to call.');
    }
    if (args['group'] == true && _retainedLeaves.containsKey(bareJid(jid))) {
      throw StateError('The retained room call is still closing.');
    }
    final generation = context.generation;
    final epoch = ++_epoch;
    _sid = _id();
    _peer = bareJid(jid);
    _video = args['video'] == true;
    _group = args['group'] == true;
    _phase = 'connecting';
    _error = null;
    _accepting = false;
    _hand = false;
    _activePresence = false;
    _started = null;
    final sid = _sid;
    _publish();
    try {
      await _requestCapture(_video);
      _guard(epoch, generation, sid);
      await _startForeground(epoch, generation, sid, video: _video);
      _guard(epoch, generation, sid);
      _armTimeout(epoch, generation, sid);
      if (_group) {
        await _startGroup(epoch, generation, sid);
      } else {
        _phase = 'outgoing';
        _publish();
        await _rpc('send_call_propose', {
          'peer_bare_jid': _peer,
          'sid': sid,
          'audio': true,
          'video': _video,
        });
        _guard(epoch, generation, sid);
      }
    } catch (_) {
      if (_current(epoch, generation)) {
        await _hangUp(reason: 'failed_application');
        _error = 'The call could not be started.';
        _publish();
      }
      rethrow;
    }
  }

  void _armTimeout(int epoch, int generation, String sid) {
    _timeout?.cancel();
    _timeout = Timer(setupTimeout, () {
      if (_current(epoch, generation, sid) && _phase != 'active') {
        unawaited(_hangUp(reason: 'timeout').catchError((Object _) {}));
      }
    });
  }

  Future<void> _startGroup(int epoch, int generation, String sid) async {
    final room = _peer;
    final cached = runtimeMap(_cache[room]);
    if (cached['terminate_pending'] != true &&
        cached['self_full_jid'] == _ownFull &&
        cached['join'] is Map) {
      var valid = true;
      try {
        validateMediaGrant(runtimeMap(cached['join']));
        valid = runtimeMap(cached['join'])['room'] == room;
      } catch (_) {
        valid = false;
      }
      if (valid && cached['sid'] is String) {
        _sid = cached['sid'] as String;
        _activePresence = true;
        _armTimeout(epoch, generation, _sid);
        await _sendPresence(_sid, epoch, generation, active: true);
        await _activate(runtimeMap(cached['join']), epoch, generation, _sid);
        return;
      }
    }
    // Register before sending so a fast echo cannot be missed; cancel on any failed send.
    final echo = Completer<void>();
    final echoSubscription = _presenceChanges.stream.listen((_) {
      if (_selfPreparing(room) && !echo.isCompleted) {
        echo.complete();
      }
    });
    try {
      await _sendPresence(sid, epoch, generation, preparing: true);
      if (!_selfPreparing(room)) {
        await echo.future.timeout(const Duration(seconds: 10));
      }
    } finally {
      await echoSubscription.cancel();
    }
    _guard(epoch, generation, sid);
    if (_otherPreparing(room)) {
      await _presenceChanges.stream
          .firstWhere((_) => !_otherPreparing(room))
          .timeout(const Duration(seconds: 10));
    }
    _guard(epoch, generation, sid);
    await _sendPresence(sid, epoch, generation, active: true);
    _activePresence = true;
    _mixerAccept = Completer<Map<String, dynamic>>();
    await _rpc('send_muji_session_initiate', {
      'room_jid': room,
      'initiator_full_jid': _ownFull,
      'sid': sid,
      'video': _video,
    });
    _guard(epoch, generation, sid);
    final join = await _mixerAccept!.future.timeout(setupTimeout);
    _guard(epoch, generation, sid);
    validateMediaGrant(join);
    if (_fullKey(join['identity']?.toString() ?? '') != _fullKey(_ownFull)) {
      throw StateError('The media grant belongs to another device.');
    }
    final cache = _cache;
    cache[room] = {
      'sid': sid,
      'self_full_jid': _ownFull,
      'video': _video,
      'join': join,
      'terminate_pending': false,
    };
    context.persistent['callSessions'] = cache;
    await context.persist();
    _guard(epoch, generation, sid);
    await _activate(join, epoch, generation, sid);
  }

  bool _selfPreparing(String room) =>
      runtimeMap(_rooms[room]?[_nick]?['muji'])['preparing'] == true;
  bool _otherPreparing(String room) => (_rooms[room] ?? {}).entries.any(
    (entry) =>
        entry.key != _nick &&
        runtimeMap(entry.value['muji'])['preparing'] == true,
  );

  Future<void> _sendPresence(
    String sid,
    int epoch,
    int generation, {
    bool active = false,
    bool preparing = false,
    bool leaving = false,
    String? roomJid,
  }) async {
    final room = roomJid ?? _peer, nick = _nick;
    final previous = _presenceTail;
    final done = Completer<void>();
    _presenceTail = done.future;
    await previous;
    try {
      requireGeneration(context, generation);
      // A teardown may clear its own old marker, but never a successor's room marker.
      if (!_current(epoch, generation, sid) &&
          _peer == room &&
          _group &&
          _phase != 'ended') {
        return;
      }
      await _rpc('update_muji_presence', {
        'room_jid': room,
        'nick': nick,
        'active': active,
        'preparing': preparing,
        'video': active && _video,
        'flags': {
          'hand_raised': active && _hand,
          'muted': active && media.snapshot['microphoneEnabled'] != true,
        },
      });
      if (!leaving) {
        _guard(epoch, generation, sid);
      }
    } finally {
      done.complete();
    }
  }

  Future<void> _accept(String expectedSid) async {
    if (_phase != 'incoming' || _sid != expectedSid || _accepting) {
      throw StateError('This call is no longer ringing.');
    }
    final epoch = _epoch, generation = context.generation, peer = _peer;
    await _requestCapture(_video);
    _guard(epoch, generation, expectedSid);
    if (_phase != 'incoming' || _accepting) {
      throw StateError('This call is no longer ringing.');
    }
    _accepting = true;
    _publish();
    _ForegroundOwner? acceptedOwner;
    try {
      await _startForeground(epoch, generation, expectedSid, video: _video);
      _guard(epoch, generation, expectedSid);
      acceptedOwner = _foregroundOwner;
      await _rpc('send_call_proceed', {
        'peer_full_jid': peer,
        'sid': expectedSid,
      });
      _guard(epoch, generation, expectedSid);
      _armTimeout(epoch, generation, expectedSid);
    } catch (_) {
      try {
        if (_current(epoch, generation, expectedSid) && _phase == 'active') {
          await _hangUp(reason: 'failed_application');
        } else {
          await _stopForeground(acceptedOwner);
        }
      } finally {
        if (_current(epoch, generation, expectedSid)) {
          _accepting = false;
          _publish();
        }
      }
      rethrow;
    }
  }

  Future<void> _decline(String expectedSid) async {
    if (_phase != 'incoming' || _sid != expectedSid) {
      throw StateError('This call is no longer ringing.');
    }
    await _hangUp(reason: 'decline');
  }

  Future<void> _hangUp({String reason = 'success'}) async {
    if (_phase == 'idle' || _phase == 'ended') {
      return;
    }
    final oldActivePresence = _activePresence;
    final foregroundOwner = _foregroundOwner;
    if (foregroundOwner != null) {
      _foregroundTeardowns.add(foregroundOwner);
    }
    final oldPhase = _phase,
        sid = _sid,
        peer = _peer,
        group = _group,
        accepting = _accepting;
    final epoch = ++_epoch, generation = context.generation;
    final signaling = ++_nextSignal;
    _signaling.add(signaling);
    if (_mixerAccept != null && !_mixerAccept!.isCompleted) {
      _mixerAccept!.complete({});
    }
    _phase = 'ended';
    _started = null;
    _timeout?.cancel();
    _duration?.cancel();
    _accepting = false;
    final mediaDisconnect = media.disconnect().catchError((Object _) {});
    _publish();
    try {
      if (group) {
        await _sendPresence(
          sid,
          epoch,
          generation,
          leaving: true,
          roomJid: peer,
        );
        if (oldActivePresence) {
          await _rpc('send_muji_session_terminate', {
            'room_jid': peer,
            'sid': sid,
          });
        }
        requireGeneration(context, generation);
        final cache = _cache;
        if (runtimeMap(cache[peer])['sid'] == sid) {
          cache.remove(peer);
        }
        context.persistent['callSessions'] = cache;
        await context.persist();
      } else if (oldPhase == 'incoming' && !accepting) {
        await _rpc('send_call_reject', {'peer_full_jid': peer, 'sid': sid});
      } else if (oldPhase == 'outgoing') {
        await _rpc('send_call_retract', {
          'peer_bare_jid': bareJid(peer),
          'sid': sid,
        });
      } else {
        final result = await _rpc('send_call_session_terminate_with_outcome', {
          'peer_full_jid': peer,
          'sid': sid,
          'reason': reason,
        });
        if (result == 'error') {
          throw StateError('Call teardown will need to be retried.');
        }
        if (reason == 'success') {
          await _rpc('send_call_finish', {'peer_full_jid': peer, 'sid': sid});
        } else {
          await _rpc('send_call_finish_with_reason', {
            'peer_full_jid': peer,
            'sid': sid,
            'reason': reason,
          });
        }
      }
    } catch (_) {
      if (group && context.generation == generation) {
        final cache = _cache;
        final retained = runtimeMap(cache[peer]);
        if (retained.isNotEmpty && retained['sid'] == sid) {
          retained['terminate_pending'] = true;
          cache[peer] = retained;
          context.persistent['callSessions'] = cache;
          await context.persist();
        }
      }
      rethrow;
    } finally {
      await mediaDisconnect;
      try {
        await _stopForeground(foregroundOwner);
      } finally {
        _foregroundTeardowns.remove(foregroundOwner);
        _signaling.remove(signaling);
        if (context.generation == generation) {
          _publish();
        }
      }
    }
  }

  Future<void> _foregroundOperation(Future<void> Function() operation) {
    final next = _foregroundTail
        .catchError((Object _) {})
        .then((_) => operation());
    _foregroundTail = next;
    // Keep a retryable lifetime chain without leaving an unhandled background error.
    unawaited(next.catchError((Object _) {}));
    return next;
  }

  Future<void> _startForeground(
    int epoch,
    int generation,
    String sid, {
    required bool video,
    bool refresh = false,
  }) => _foregroundOperation(() async {
    _guard(epoch, generation, sid);
    final key = '$generation:$epoch:$sid';
    if (!refresh && _foregroundOwner?.sid == key) {
      return;
    }
    final owner = _ForegroundOwner(key);
    _foregroundOwner = owner;
    try {
      await _callForeground(true, video, owner.sid);
      _guard(epoch, generation, sid);
    } catch (_) {
      if (identical(_foregroundOwner, owner)) {
        await _callForeground(false, false, owner.sid);
        _foregroundOwner = null;
      }
      rethrow;
    }
  });

  Future<void> _stopForeground(_ForegroundOwner? owner) =>
      _foregroundOperation(() async {
        if (owner == null || !identical(_foregroundOwner, owner)) {
          return;
        }
        await _callForeground(false, false, owner.sid);
        if (identical(_foregroundOwner, owner)) {
          _foregroundOwner = null;
        }
      });

  Future<void> _endRemoteMedia() async {
    final owner = _foregroundOwner;
    await media.disconnect();
    await _stopForeground(owner);
  }

  Future<void> _activate(
    Map<String, dynamic> join,
    int epoch,
    int generation,
    String sid,
  ) async {
    validateMediaGrant(join);
    if (_fullKey(join['identity']?.toString() ?? '') != _fullKey(_ownFull)) {
      throw StateError('The media grant belongs to another device.');
    }
    _guard(epoch, generation, sid);
    _phase = 'active';
    _started = DateTime.now();
    _timeout?.cancel();
    _publish();
    final ice = await context
        .rpc('fetch_external_services')
        .catchError((Object _) => <dynamic>[]);
    _guard(epoch, generation, sid);
    try {
      await _startForeground(epoch, generation, sid, video: _video);
      _guard(epoch, generation, sid);
      await media.connect(join, _video, runtimeRows(ice));
      _guard(epoch, generation, sid);
      if (_group) {
        await _sendPresence(sid, epoch, generation, active: true);
      }
    } catch (_) {
      if (_current(epoch, generation, sid)) {
        await _hangUp(reason: 'failed_application');
      }
      rethrow;
    }
    _duration?.cancel();
    _duration = Timer.periodic(const Duration(seconds: 1), (_) {
      if (_current(epoch, generation, sid)) {
        _publish();
      }
    });
    _publish();
  }

  void _mediaChanged() {
    _publish();
    if (media.snapshot['mediaState'] == 'failed' && _phase == 'active') {
      unawaited(
        _hangUp(reason: 'connectivity_error').catchError((Object _) {}),
      );
    }
  }

  void onEvent(Map<String, dynamic> event) {
    if (_disposed) {
      return;
    }
    switch (event['type']) {
      case 'presence':
        _presence(runtimeMap(event['data']));
      case 'call':
        final eventGeneration = context.generation;
        final eventEpoch = _epoch;
        unawaited(
          _call(runtimeMap(event['data'])).catchError((Object _) {
            if (!_disposed &&
                context.generation == eventGeneration &&
                _epoch == eventEpoch) {
              _error = 'The call operation failed.';
              _publish();
            }
          }),
        );
      case 'connected':
        _rooms.clear();
        if (_group && _phase == 'active') {
          unawaited(
            _sendPresence(
              _sid,
              _epoch,
              context.generation,
              active: true,
            ).catchError((Object _) {}),
          );
        }
        unawaited(_retryTerminates().catchError((Object _) {}));
    }
  }

  void _presence(Map<String, dynamic> presence) {
    final from = presence['from']?.toString() ?? '';
    if (!from.contains('/') ||
        (presence['muc_jid'] == null &&
            presence['muc_role'] == null &&
            presence['muji'] == null)) {
      return;
    }
    final room = bareJid(from), nick = from.substring(from.indexOf('/') + 1);
    final occupants = _rooms.putIfAbsent(room, () => {});
    if (presence['presence_type'] == 'unavailable') {
      occupants.remove(nick);
    } else {
      occupants[nick] = presence;
    }
    _presenceChanges.add(null);
    if (_group && _peer == room) {
      _publish();
    }
  }

  Future<void> _call(Map<String, dynamic> event) async {
    final kindData = runtimeMap(event['kind']);
    final kind = (event['kind'] is String ? event['kind'] : kindData['type'])
        ?.toString()
        .replaceAll('_', '-');
    final from = event['from']?.toString() ?? '';
    final sid = event['sid']?.toString() ?? '';
    if (from.isEmpty || sid.isEmpty) {
      return;
    }
    final mediaData = runtimeMap(event['media'] ?? kindData['media']);
    final join = runtimeMap(event['join'] ?? kindData['join']);
    final self = bareJid(from) == context.jid;
    if (self) {
      if (_fullKey(from) != _fullKey(_ownFull) &&
          sid == _sid &&
          _phase == 'incoming' &&
          {'proceed', 'reject'}.contains(kind)) {
        _epoch++;
        _phase = 'ended';
        _timeout?.cancel();
        _publish();
      }
      return;
    }
    if (kind == 'propose') {
      if (!_group && _phase == 'outgoing' && bareJid(_peer) == bareJid(from)) {
        if (sid.compareTo(_sid) < 0) {
          await _rpc('send_call_retract_tie_break', {
            'peer_full_jid': from,
            'sid': _sid,
          });
        } else {
          await _rpc('send_call_reject_tie_break', {
            'peer_full_jid': from,
            'sid': sid,
          });
          return;
        }
      } else if (_phase != 'idle' && _phase != 'ended') {
        if (_phase == 'incoming' &&
            sid == _sid &&
            _fullKey(from) == _fullKey(_peer)) {
          return;
        }
        await _rpc('send_call_reject', {'peer_full_jid': from, 'sid': sid});
        return;
      }
      _epoch++;
      _phase = 'incoming';
      _sid = sid;
      _peer = from;
      _group = false;
      _video = mediaData['video'] == true;
      _accepting = false;
      _error = null;
      _started = null;
      _timeout?.cancel();
      _publish();
      await _rpc('send_call_ringing', {
        'peer_bare_jid': bareJid(from),
        'sid': sid,
      });
      return;
    }
    if (sid != _sid) {
      return;
    }
    final epoch = _epoch, generation = context.generation;
    if (_group) {
      final mixer = 'calls.${context.jid?.split('@').last}';
      if (bareJid(from) != mixer) {
        return;
      }
      if (kind == 'session-accept' &&
          _phase == 'connecting' &&
          join.isNotEmpty &&
          join['room'] == _peer) {
        if (_mixerAccept != null && !_mixerAccept!.isCompleted) {
          _mixerAccept!.complete(join);
        }
      } else if (kind == 'session-terminate') {
        _epoch++;
        _phase = 'ended';
        _timeout?.cancel();
        _duration?.cancel();
        if (_mixerAccept != null && !_mixerAccept!.isCompleted) {
          _mixerAccept!.complete({});
        }
        await _endRemoteMedia();
        _publish();
      }
      return;
    }
    if (bareJid(from) != bareJid(_peer)) {
      return;
    }
    switch (kind) {
      case 'ringing':
        _publish();
      case 'proceed':
        if (_phase != 'outgoing' || !from.contains('/')) {
          return;
        }
        _peer = from;
        _phase = 'connecting';
        _publish();
        await _rpc('send_call_session_initiate', {
          'peer_full_jid': from,
          'initiator_full_jid': _ownFull,
          'sid': sid,
          'audio': true,
          'video': _video,
        });
        _guard(epoch, generation, sid);
      case 'session-initiate':
        if (_phase != 'incoming' ||
            !_accepting ||
            _fullKey(from) != _fullKey(_peer) ||
            join.isEmpty) {
          return;
        }
        validateMediaGrant(join);
        await _rpc('send_call_session_accept', {
          'peer_full_jid': from,
          'responder_full_jid': _ownFull,
          'sid': sid,
          'audio': true,
          'video': _video,
        });
        _guard(epoch, generation, sid);
        await _activate(join, epoch, generation, sid);
      case 'session-accept':
        if (_phase != 'connecting' ||
            _fullKey(from) != _fullKey(_peer) ||
            join.isEmpty) {
          return;
        }
        await _activate(join, epoch, generation, sid);
      case 'reject':
      case 'retract':
      case 'finish':
      case 'session-terminate':
        if (_peer.contains('/') && _fullKey(from) != _fullKey(_peer)) {
          return;
        }
        _epoch++;
        _phase = 'ended';
        _started = null;
        _timeout?.cancel();
        _duration?.cancel();
        await _endRemoteMedia();
        _publish();
    }
  }

  Map<String, dynamic> _roomState(String value) {
    final room = bareJid(value), entries = _rooms[room] ?? {};
    final participants = entries.entries
        .where((entry) => runtimeMap(entry.value['muji'])['active'] == true)
        .map((entry) => entry.key)
        .toList();
    final local =
        _group && _peer == room && {'connecting', 'active'}.contains(_phase);
    final cached = runtimeMap(_cache[room]);
    final own = entries[_nick];
    final owner = own?['muc_jid']?.toString();
    final deadOwner =
        owner == null ||
        _fullKey(owner) == _fullKey(_ownFull) ||
        _fullKey(owner) == _fullKey(cached['self_full_jid']?.toString() ?? '');
    final retained =
        !local &&
        (cached['terminate_pending'] == true ||
            (participants.contains(_nick) && deadOwner));
    final busy = !{'idle', 'ended'}.contains(_phase) && !local;
    return {
      'jid': room,
      'participants': participants,
      'participantCount': participants.length,
      'localCall': local,
      'video': entries.values.any(
        (entry) => runtimeMap(entry['muji'])['video'] == true,
      ),
      'canLeaveRetained': retained,
      'busy': busy,
      'controls': local || busy
          ? 'hidden'
          : participants.isEmpty
          ? 'start'
          : 'join',
    };
  }

  Future<void> _leaveRetained(String value) async {
    final room = bareJid(value), generation = context.generation;
    if (_retainedLeaves.containsKey(room) ||
        _roomState(room)['canLeaveRetained'] != true) {
      throw StateError('This room has no retained call to leave.');
    }
    final token = Object();
    _retainedLeaves[room] = token;
    final cached = runtimeMap(_cache[room]);
    final previous = _presenceTail;
    final done = Completer<void>();
    _presenceTail = done.future;
    try {
      await previous;
      requireGeneration(context, generation);
      await _rpc('update_muji_presence', {
        'room_jid': room,
        'nick': _nick,
        'active': false,
        'preparing': false,
        'video': false,
        'flags': {'hand_raised': false, 'muted': false},
      });
    } catch (_) {
      if (identical(_retainedLeaves[room], token)) {
        _retainedLeaves.remove(room);
      }
      rethrow;
    } finally {
      done.complete();
    }
    try {
      requireGeneration(context, generation);
      if (cached['sid'] is String) {
        await _rpc('send_muji_session_terminate', {
          'room_jid': room,
          'sid': cached['sid'],
        });
      }
      requireGeneration(context, generation);
      final cache = _cache;
      if (runtimeMap(cache[room])['sid'] == cached['sid']) {
        cache.remove(room);
      }
      context.persistent['callSessions'] = cache;
      await context.persist();
      requireGeneration(context, generation);
      _rooms[room]?.remove(_nick);
    } finally {
      if (identical(_retainedLeaves[room], token)) {
        _retainedLeaves.remove(room);
      }
    }
  }

  Future<void> _retryTerminates() async {
    final generation = context.generation;
    if (!_cache.values.any(
      (value) => runtimeMap(value)['terminate_pending'] == true,
    )) {
      return;
    }
    final signaling = ++_nextSignal;
    _signaling.add(signaling);
    _publish();
    try {
      for (final entry in _cache.entries.toList()) {
        final cached = runtimeMap(entry.value);
        if (cached['terminate_pending'] != true || cached['sid'] is! String) {
          continue;
        }
        if (_group && _peer == entry.key && _phase == 'active') {
          continue;
        }
        await _rpc('send_muji_session_terminate', {
          'room_jid': entry.key,
          'sid': cached['sid'],
        });
        requireGeneration(context, generation);
        final cache = _cache;
        if (runtimeMap(cache[entry.key])['sid'] == cached['sid']) {
          cache.remove(entry.key);
        }
        context.persistent['callSessions'] = cache;
        await context.persist();
      }
    } finally {
      _signaling.remove(signaling);
      if (context.generation == generation) {
        _publish();
      }
    }
  }

  void onAccountChanged() {
    final tearingDown = _foregroundTeardowns.contains(_foregroundOwner);
    _epoch++;
    _signaling.clear();
    _retainedLeaves.clear();
    _phase = 'idle';
    _sid = '';
    _peer = '';
    _error = null;
    _group = false;
    _hand = false;
    _accepting = false;
    _timeout?.cancel();
    _duration?.cancel();
    _rooms.clear();
    _started = null;
    if (_mixerAccept != null && !_mixerAccept!.isCompleted) {
      _mixerAccept!.complete({});
    }
    _mixerAccept = null;
    final owner = _foregroundOwner;
    unawaited(
      () async {
        try {
          await media.disconnect();
        } finally {
          if (!tearingDown) {
            await _stopForeground(owner);
          }
        }
      }().catchError((Object _) {}),
    );
    _publish();
  }

  Future<void> dispose() async {
    _disposed = true;
    _epoch++;
    _timeout?.cancel();
    _duration?.cancel();
    media.removeListener(_mediaChanged);
    try {
      await media.disconnect();
    } finally {
      await _stopForeground(_foregroundOwner);
    }
    media.dispose();
    await _presenceChanges.close();
    await _events.close();
  }
}

String _id() => List.generate(
  16,
  (_) => Random.secure().nextInt(256).toRadixString(16).padLeft(2, '0'),
).join();

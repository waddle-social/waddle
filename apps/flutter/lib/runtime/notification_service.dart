import 'dart:async';
import 'dart:convert';

import 'package:cryptography/cryptography.dart';

import 'context.dart';
import 'session_auth.dart';
import 'notification_backend.dart';
import 'notification_backend_stub.dart'
    if (dart.library.io) 'notification_backend_io.dart'
    if (dart.library.js_interop) 'notification_backend_web.dart'
    as platform;

export 'notification_backend.dart';

typedef NotificationReply =
    Future<void> Function({
      required String jid,
      required bool group,
      required String body,
      String? threadId,
    });
typedef NotificationCallAction =
    Future<void> Function(String action, String sid);

Future<NotificationBackend> initializeNotificationForwarder() =>
    platform.initializeForwarder();

/// The decrypted v1 push envelope carries identifiers and counts, never content.
class NotificationEnvelope {
  const NotificationEnvelope({
    required this.kind,
    required this.conversation,
    required this.item,
    this.thread,
    this.unread,
  });
  static const kinds = {
    'dm',
    'dm_mention',
    'personal_mention',
    'channel_mention',
    'active_channel_mention',
    'notify_all',
  };
  final String kind, conversation, item;
  final String? thread;
  final int? unread;
  bool get group => kind != 'dm' && kind != 'dm_mention';
  bool get mention => kind != 'dm' && kind != 'notify_all';
  Map<String, dynamic> toJson() => {
    'v': 1,
    'class': kind,
    'conversation': conversation,
    'item': item,
    if (thread != null) 'thread': thread,
    if (unread != null) 'unread': unread,
  };

  static NotificationEnvelope? parse(dynamic content) {
    try {
      final dynamic value = content is List<int>
          ? jsonDecode(utf8.decode(content).replaceFirst(RegExp(r'\x00+$'), ''))
          : content is String
          ? jsonDecode(content)
          : content;
      final map = runtimeMap(value);
      final kind = map['class'], jid = map['conversation'], item = map['item'];
      if (map['v'] != 1 ||
          kind is! String ||
          !kinds.contains(kind) ||
          jid is! String ||
          !plausibleJid(jid) ||
          item is! String ||
          item.trim().isEmpty ||
          item.length > 4096) {
        return null;
      }
      final thread = map['thread'];
      if (thread != null && (thread is! String || thread.length > 4096)) {
        return null;
      }
      return NotificationEnvelope(
        kind: kind,
        conversation: jid,
        item: item,
        thread: thread as String?,
        unread: map['unread'] is int ? map['unread'] as int : null,
      );
    } catch (_) {
      return null;
    }
  }

  static bool plausibleJid(String value) =>
      value.isNotEmpty &&
      value.length <= 3071 &&
      !RegExp(r'[/\s\x00-\x1f\x7f]').hasMatch(value) &&
      '@'.allMatches(value).length <= 1 &&
      !value.startsWith('@') &&
      !value.endsWith('@');
}

class NotificationService {
  NotificationService(
    this.context, {
    NotificationBackend? backend,
    this.selectDistributor,
    this.reply,
    this.callAction,
    this.initiallyVisible = false,
    this.liveGrace = const Duration(seconds: 5),
    Future<void> Function(Duration)? delay,
  }) : _backend = backend ?? platform.createBackend(),
       _delay = delay ?? Future<void>.delayed;

  final RuntimeContext context;
  final NotificationBackend _backend;
  final Future<String?> Function(List<String>)? selectDistributor;
  final NotificationReply? reply;
  final NotificationCallAction? callAction;
  final bool initiallyVisible;
  final Duration liveGrace;
  final Future<void> Function(Duration) _delay;
  final _events = StreamController<Map<String, dynamic>>.broadcast();
  final _seen = <String>{}, _live = <String>{};
  final _modes = <String, String>{};
  final _pending = <Map<String, dynamic>>[];
  Future<void> _tail = Future<void>.value();
  Future<void> _callTail = Future<void>.value();
  Future<void> _callNoticeTail = Future<void>.value();
  String? _callForegroundSid;
  Map<String, dynamic> _callState = {};
  String? _incomingNoticeSid;
  int _callRevision = 0;
  final _pendingCallActions = <Map<String, dynamic>>[];
  final _callActionsInFlight = <String>{};
  bool _visible = false,
      _disposed = false,
      _connected = false,
      _initialized = false;
  bool _background = false, _resolved = false, _permissionGranted = false;
  String? _owner, _instance;
  int _epoch = 0;
  Stream<Map<String, dynamic>> get events => _events.stream;
  Future<void> get idle => _tail;
  FcmNotifications? get _fcm =>
      _backend is FcmNotifications ? _backend as FcmNotifications : null;
  bool get _useFcm {
    if (_fcm?.fcmAvailable != true) return false;
    final selected = _state['transport'];
    if (selected != null) return selected == 'fcm';
    // An installed distributor with a registered endpoint remains the account's choice.
    return runtimeMap(_state['endpoint'])['endpoint'] == null;
  }

  Future<void> _unsubscribe(String instance) async {
    if (_fcm?.fcmAvailable == true) await _fcm!.retireFcm(instance);
    await _backend.unsubscribe(instance);
  }

  Future<void> initialize() async {
    if (_initialized) return;
    _initialized = true;
    _visible = initiallyVisible;
    await _backend.initialize((event) => _schedule(handleOwnerEvent(event)));
    await _fcm?.initializeFcm(handleOwnerEvent);
    onAccountChanged();
    _snapshot();
  }

  Future<T> _serial<T>(Future<T> Function() action) {
    final next = _tail.catchError((Object _) {}).then((_) => action());
    _tail = next.then<void>((_) {}, onError: (Object _, StackTrace _) {});
    return next;
  }

  void _schedule(Future<void> work) => unawaited(
    work.catchError((Object _) {
      if (!_disposed) _snapshot('Background delivery temporarily unavailable');
    }),
  );

  Map<String, dynamic> get _state {
    final stored = context.persistent['push'];
    if (stored is Map<String, dynamic>) return stored;
    final state = runtimeMap(stored);
    context.persistent['push'] = state;
    return state;
  }

  bool _current(int epoch, String owner) =>
      !_disposed && epoch == _epoch && context.jid == owner && _owner == owner;
  void _emit(String topic, Map<String, dynamic> data) {
    if (!_disposed) {
      _events.add({
        'topic': topic,
        'data': data,
        'generation': context.generation,
      });
    }
  }

  void _snapshot([String? status]) => _emit('delivery', {
    'resolved': _resolved,
    'pushEnabled':
        _backend.supported && _state['enabled'] == true && _permissionGranted,
    'localDeliveryOnly': _backend.localDeliveryOnly,
    'fcmAvailable': _fcm?.fcmAvailable == true,
    'stayConnectedSupported': _backend.supportsBackground,
    'stayConnected': _background,
    'distributorLabel': _backend.localDeliveryOnly
        ? 'Desktop notifications'
        : _backend.supported
        ? (_useFcm ? 'Google Play push' : 'Web Push')
        : 'Push unavailable on this platform',
    'status':
        status ??
        (_backend.localDeliveryOnly
            ? (_permissionGranted
                  ? 'Receive notifications while Waddle is running'
                  : 'Desktop notification service unavailable')
            : !_permissionGranted
            ? 'Notifications are blocked in system settings'
            : _state['enabled'] == true
            ? 'Push delivery enabled'
            : 'Choose a push distributor'),
  });

  void onAccountChanged() {
    final oldInstance = _instance;
    _epoch++;
    _owner = context.jid;
    _instance = null;
    _connected = false;
    _resolved = false;
    _permissionGranted = false;
    _seen.clear();
    _live.clear();
    _modes.clear();
    _callState = {};
    _callRevision++;
    _incomingNoticeSid = null;
    if (_initialized) _schedule(_backend.cancel(9003));
    if (_owner == null && context.session['state'] == 'signedOut') {
      _pending.clear();
      _pendingCallActions.clear();
      if (_background) {
        _background = false;
        _schedule(_backend.background(false).then<void>((_) {}));
      }
    }
    _schedule(_backend.account(null, null, enabled: false, visible: _visible));
    _schedule(_backend.clear());
    if (oldInstance != null && _owner == null) {
      _schedule(_unsubscribe(oldInstance));
    }
    final owner = _owner, epoch = _epoch;
    if (owner != null) {
      _schedule(
        _serial(() async {
          final instance = await _accountInstance(owner);
          if (!_current(epoch, owner)) return;
          _instance = instance;
          for (final item
              in context.persistent['pushAnnouncedItems'] as List? ??
                  const []) {
            if (item is String) _remember(_seen, item);
          }
          await _backend.account(
            owner,
            _instance,
            enabled: context.preferences['notificationsEnabled'] != false,
            visible: _visible,
            sound: context.preferences['messageSoundsEnabled'] != false,
          );
          await _ensurePermission();
          if (_connected) await _reconcile();
          await _flushPending();
        }),
      );
    }
  }

  void onEvent(Map<String, dynamic> event) {
    if (_disposed) return;
    final type = event['type'];
    if (type == 'connected') {
      _connected = true;
      _schedule(
        _serial(() async {
          await _loadModes();
          await _reconcile();
          await _flushPending();
        }),
      );
    } else if (type == 'disconnected') {
      _connected = false;
    } else if (type == 'message') {
      final data = runtimeMap(event['data']);
      _schedule(_serial(() => _liveMessage(data)));
    }
  }

  Future<void> _loadModes() async {
    final owner = _owner, epoch = _epoch;
    if (owner == null) return;
    for (final verb in ['fetch_user_bookmarks', 'fetch_dm_bookmarks']) {
      try {
        final rows = runtimeRows(await context.rpc(verb));
        if (!_current(epoch, owner)) return;
        for (final row in rows) {
          final jid = row['room_jid'] ?? row['peer_jid'] ?? row['jid'];
          if (jid is String && row['notify_mode'] is String) {
            _modes[jid] = row['notify_mode'] as String;
          }
        }
      } catch (_) {
        /* Keep the last authoritative preference during an outage. */
      }
    }
  }

  Future<void> _liveMessage(Map<String, dynamic> data) async {
    final owner = _owner, instance = _instance, epoch = _epoch;
    if (owner == null ||
        instance == null ||
        !_current(epoch, owner) ||
        !_permissionGranted ||
        _visible ||
        context.preferences['notificationsEnabled'] == false) {
      return;
    }
    if (data['body'] is! String ||
        (data['body'] as String).isEmpty ||
        data['carbon'] == 'sent') {
      return;
    }
    if ([
          'replaces_id',
          'reaction_target_id',
          'retracts_id',
          'retraction_id',
          'moderation_target_id',
          'pin_event',
          'call_thread_ended',
          'safety_scores',
        ].any((key) => data[key] != null) ||
        data['is_retracted'] == true ||
        runtimeRows(data['reactions']).isNotEmpty) {
      return;
    }
    final group = data['message_type'] == 'groupchat' || data['is_muc'] == true;
    if (!group &&
        (data['message_type'] != 'chat' || data['muc_user'] == true)) {
      return;
    }
    final from = '${data['from'] ?? ''}';
    final conversation = from.split('/').first;
    if (!NotificationEnvelope.plausibleJid(conversation) ||
        !group && conversation == owner) {
      return;
    }
    if (group &&
        from.contains('/') &&
        from.substring(from.indexOf('/') + 1) ==
            context.session['xmppLocalpart']) {
      return;
    }
    final authority = group ? conversation : owner;
    String? item;
    for (final sid in runtimeRows(data['stanza_ids'])) {
      if (sid['by'] == authority && sid['id'] is String) {
        item = sid['id'] as String;
        break;
      }
    }
    if (item == null &&
        data['stanza_id_by'] == authority &&
        data['stanza_id'] is String) {
      item = data['stanza_id'] as String;
    }
    if (item == null || item.isEmpty || _seen.contains(item)) return;
    final mention =
        data['broadcast_mention'] != null ||
        (data['mention_uris'] as List? ?? const []).any((value) {
          final uri = Uri.tryParse('$value');
          return uri?.scheme == 'xmpp' && uri?.path == owner;
        });
    final saved = runtimeMap(context.persistent['notificationModes']);
    final mode = saved[conversation] ?? _modes[conversation] ?? 'always';
    if (mode == 'never' || mode == 'on_mention' && !mention) return;
    final envelope = NotificationEnvelope(
      kind: group
          ? (mention ? 'personal_mention' : 'notify_all')
          : (mention ? 'dm_mention' : 'dm'),
      conversation: conversation,
      item: item,
      thread: data['thread'] as String?,
    );
    await _backend.show(
      NotificationNotice(
        id: _noticeId(conversation),
        title: conversation,
        body: mention ? 'Mentioned you' : 'New message',
        payload: {'owner': owner, 'instance': instance, ...envelope.toJson()},
        sound: context.preferences['messageSoundsEnabled'] != false,
      ),
    );
    if (!_current(epoch, owner) ||
        _visible ||
        context.preferences['notificationsEnabled'] == false) {
      await _backend.clear();
      return;
    }
    _remember(_live, item);
    _remember(_seen, item);
    context.persistent['pushAnnouncedItems'] = _seen.toList();
    await context.persist();
  }

  Future<dynamic> invoke(
    String method, [
    Map<String, dynamic> args = const {},
  ]) async {
    if (_disposed) throw StateError('Notifications are closed.');
    switch (method) {
      case 'app.visibility':
        _visible = args['visible'] == true;
        if (_visible && _owner != null) await _ensurePermission();
        await _backend.account(
          _owner,
          _instance,
          enabled: context.preferences['notificationsEnabled'] != false,
          visible: _visible,
          sound: context.preferences['messageSoundsEnabled'] != false,
        );
        if (_visible) await _backend.clear();
        return null;
      case 'push.settings':
        await _backend.settings();
        return null;
      case 'push.fcm':
        return _selectPushTransport(fcm: true);
      case 'push.callForeground':
        return _setCallForeground(args);
      case 'push.callState':
        return _updateCallState(args);
      case 'push.choose':
        return _selectPushTransport(fcm: false);
      case 'push.retire':
        final owner = _owner, instance = _instance, epoch = _epoch;
        return _serial(() async {
          if (owner == null || !_current(epoch, owner)) return;
          await _retireDevice();
          if (!_current(epoch, owner)) return;
          if (instance != null) await _unsubscribe(instance);
          if (_current(epoch, owner)) await _backend.clear();
        });
      case 'push.reconcile':
        return _serial(() => _reconcile(userInitiated: true));
      default:
        throw UnsupportedError('Unknown notification command: $method');
    }
  }

  Future<dynamic> _selectPushTransport({required bool fcm}) async {
    final owner = _owner, epoch = _epoch, instance = _instance;
    if (owner == null || instance == null) throw StateError('Sign in first.');
    if (fcm && _fcm?.fcmAvailable != true) {
      throw UnsupportedError('Google Play push is unavailable.');
    }
    final permission = await _backend.requestPermission();
    if (!_current(epoch, owner)) return null;
    _permissionGranted = permission;
    if (!permission) {
      throw StateError('Allow notifications in your device settings.');
    }
    String? selected;
    if (!fcm) {
      final options = await _backend.distributors();
      if (!_current(epoch, owner)) return null;
      if (options.isEmpty) {
        throw UnsupportedError(
          'Install a UnifiedPush distributor to receive background messages.',
        );
      }
      selected = options.length == 1
          ? options.single
          : await selectDistributor?.call(options);
      if (!_current(epoch, owner)) return null;
      if (selected == null) return options;
      if (!options.contains(selected)) {
        throw ArgumentError('Unknown push distributor.');
      }
    }
    return _serial(() async {
      if (!_current(epoch, owner)) return;
      if (selected != null) await _backend.selectDistributor(selected);
      if (!_current(epoch, owner)) return;
      if (_useFcm != fcm) {
        await _retireDevice();
        if (!_current(epoch, owner)) return;
        if (_useFcm) {
          await _fcm!.retireFcm(instance);
        } else {
          await _backend.unsubscribe(instance);
        }
        if (!_current(epoch, owner)) return;
        _state.remove('endpoint');
      }
      _state['transport'] = fcm ? 'fcm' : 'unifiedpush';
      await context.persist();
      if (_current(epoch, owner)) await _reconcile(userInitiated: true);
    });
  }

  Future<void> _ensurePermission() async {
    final owner = _owner, epoch = _epoch;
    if (owner == null) return;
    final previous = _permissionGranted;
    final allowed = await _backend.ensurePermission(
      requestOnce: _visible && context.session['state'] == 'ready',
    );
    if (!_current(epoch, owner)) return;
    _permissionGranted = allowed;
    if (allowed != previous) _snapshot();
    if (allowed && !previous && _callState['state'] == 'incoming') {
      _incomingNoticeSid = null;
      _schedule(_updateCallState(Map.of(_callState)));
    }
  }

  Future<String> _accountInstance(String owner) async {
    final origin = context.session['serverOrigin'];
    // Keep the default deployment's existing registrations on upgrade.
    final identity =
        origin == null || origin == HttpAuthGateway.defaultBaseUri.origin
        ? owner
        : '$origin\n$owner';
    final digest = await Sha256().hash(utf8.encode(identity));
    return 'waddle-flutter-${digest.bytes.take(8).map((byte) => byte.toRadixString(16).padLeft(2, '0')).join()}';
  }

  Future<void> _updateCallState(Map<String, dynamic> state) {
    if (state['generation'] is int &&
        state['generation'] != context.generation) {
      return Future<void>.value();
    }
    _callState = Map.of(state);
    if ((state['state'] != 'incoming' || state['accepting'] == true) &&
        _incomingNoticeSid != null) {
      _incomingNoticeSid = null;
      _schedule(_backend.cancel(9003));
    }
    final revision = ++_callRevision;
    final owner = _owner, epoch = _epoch;
    final operation = _callNoticeTail.catchError((Object _) {}).then((_) async {
      final sid = state['sid'];
      if (!_permissionGranted ||
          state['state'] != 'incoming' ||
          state['accepting'] == true ||
          sid is! String ||
          sid.isEmpty ||
          owner == null ||
          !_current(epoch, owner)) {
        if (_incomingNoticeSid != null) await _backend.cancel(9003);
        _incomingNoticeSid = null;
      } else if (_incomingNoticeSid != sid) {
        final instance = await _accountInstance(owner);
        if (revision != _callRevision || !_current(epoch, owner)) return;
        if (_incomingNoticeSid != null) await _backend.cancel(9003);
        final peer = '${state['peerJid'] ?? state['peer'] ?? 'Caller'}';
        _incomingNoticeSid = sid;
        await _backend.show(
          NotificationNotice(
            id: 9003,
            title: '${state['peerName'] ?? peer.split('/').first}',
            body: state['video'] == true
                ? 'Incoming video call'
                : 'Incoming voice call',
            incomingCall: true,
            payload: {
              'kind': 'call',
              'owner': owner,
              'instance': instance,
              'sid': sid,
            },
          ),
        );
        if (revision != _callRevision || !_current(epoch, owner)) {
          await _backend.cancel(9003);
          _incomingNoticeSid = null;
        }
      }
      if (revision != _callRevision || !_current(epoch, owner ?? '')) return;
      final pending = List<Map<String, dynamic>>.of(_pendingCallActions);
      _pendingCallActions.clear();
      for (final event in pending) {
        await _handleCallAction(event);
      }
    });
    _callNoticeTail = operation;
    return operation;
  }

  Future<void> _handleCallAction(Map<String, dynamic> event) async {
    if (_disposed || context.session['state'] == 'signedOut') return;
    final payload = runtimeMap(event['payload']);
    final sid = payload['sid'];
    final owner = _owner, epoch = _epoch;
    if (sid is! String || sid.isEmpty) return;
    if (owner == null || context.session['state'] != 'ready') {
      if (_pendingCallActions.length < 8) _pendingCallActions.add(event);
      return;
    }
    if (payload['owner'] != owner ||
        payload['instance'] != await _accountInstance(owner) ||
        !_current(epoch, owner)) {
      return;
    }
    if (_callState.isEmpty) {
      if (_pendingCallActions.length < 8) _pendingCallActions.add(event);
      return;
    }
    if (_callState['state'] != 'incoming' ||
        _callState['accepting'] == true ||
        _callState['sid'] != sid ||
        _callActionsInFlight.contains(sid)) {
      return;
    }
    final action = event['action'];
    if (action == 'open' || action == 'answer') {
      await _present(event);
      if (!_current(epoch, owner) || _callState['sid'] != sid) return;
    }
    // Opening/full-screen display never implicitly accepts a call.
    if (action != 'answer' && action != 'decline' || callAction == null) return;
    _callActionsInFlight.add(sid);
    try {
      await callAction!(action as String, sid);
    } finally {
      _callActionsInFlight.remove(sid);
    }
  }

  Future<void> _setCallForeground(Map<String, dynamic> args) {
    final key = args['sid'];
    if (key is! String || key.isEmpty) {
      throw ArgumentError('A call ownership key is required.');
    }
    final owner = _owner, epoch = _epoch;
    final active = args['active'] == true;
    final operation = _callTail.catchError((Object _) {}).then((_) async {
      if (!active) {
        if (_callForegroundSid != key) return;
        await _backend.callForeground(false);
        _callForegroundSid = null;
        return;
      }
      if (!_initialized || owner == null || !_current(epoch, owner)) {
        throw const AccountChanged();
      }
      _callForegroundSid = key;
      try {
        await _backend.callForeground(true, video: args['video'] == true);
        if (!_current(epoch, owner)) throw const AccountChanged();
      } catch (_) {
        if (_callForegroundSid == key) {
          await _backend.callForeground(false);
          _callForegroundSid = null;
        }
        rethrow;
      }
    });
    _callTail = operation;
    return operation;
  }

  Future<void> _reconcile({bool userInitiated = false}) async {
    final owner = _owner, instance = _instance, epoch = _epoch;
    if (owner == null || instance == null || !_current(epoch, owner)) return;
    await _backend.account(
      owner,
      instance,
      enabled: context.preferences['notificationsEnabled'] != false,
      visible: _visible,
      sound: context.preferences['messageSoundsEnabled'] != false,
    );
    await _ensurePermission();
    await _updateBackground(userInitiated: userInitiated);
    if (!_current(epoch, owner)) return;
    if (context.preferences['notificationsEnabled'] == false) {
      await _retireDevice();
      if (_current(epoch, owner)) await _unsubscribe(instance);
      _resolved = true;
      _snapshot('Notifications disabled');
      return;
    }
    if (!_connected) return;
    if (!_backend.supported) {
      _resolved = true;
      _snapshot();
      return;
    }
    final service = 'push.${owner.split('@').last}';
    if (runtimeRows(_state['pendingDisables']).isNotEmpty) {
      await _drainPending();
      if (!_current(epoch, owner) ||
          runtimeRows(_state['pendingDisables']).isNotEmpty) {
        return;
      }
    }
    if (_useFcm) {
      _state['service'] = service;
      late final Map<String, dynamic> credentials;
      try {
        credentials = await _fcm!.subscribeFcm(instance);
      } catch (_) {
        if (_current(epoch, owner)) {
          _resolved = true;
          _snapshot('Google Play push is unavailable on this device');
        }
        return;
      }
      if (!_current(epoch, owner)) return;
      await _handle({
        'type': 'endpoint',
        'instance': instance,
        'transport': 'fcm',
        'endpoint': credentials,
      });
      return;
    }
    final rawLookup = await context.rpc('fetch_push_vapid_public_key', {
      'push_service_jid': service,
    });
    final lookup = runtimeMap(rawLookup);
    if (!_current(epoch, owner)) return;
    if (rawLookup == null ||
        lookup['type'] == 'not_advertised' ||
        lookup['kind'] == 'not_advertised') {
      await _retireDevice();
      if (!_current(epoch, owner)) return;
      await _backend.unsubscribe(instance);
      _resolved = true;
      _snapshot('This server does not advertise Web Push');
      return;
    }
    final key = lookup['public_key'] ?? lookup['publicKey'];
    if (key is! String || key.isEmpty) {
      _snapshot('Push service temporarily unavailable');
      return;
    }
    if (_state['vapidKid'] != lookup['kid']) {
      await _retireDevice();
      if (!_current(epoch, owner)) return;
      _state.remove('endpoint');
      _state['vapidKid'] = lookup['kid'];
    }
    _state['service'] = service;
    await context.persist();
    if (!_current(epoch, owner)) return;
    if (await _backend.distributor() == null) {
      _resolved = true;
      _snapshot();
      return;
    }
    if (!_current(epoch, owner)) return;
    await _backend.subscribe(instance, key);
  }

  Future<void> _updateBackground({bool userInitiated = false}) async {
    final owner = _owner, epoch = _epoch;
    final preference = context.preferences['stayConnected'];
    final fallback =
        preference == null &&
        !(_state['enabled'] == true && _permissionGranted);
    final acquired = await _backend.background(
      preference == true || fallback,
      requestPermission: userInitiated && _visible,
    );
    if (owner == null || !_current(epoch, owner)) {
      if (acquired) await _backend.background(false);
      _background = false;
      return;
    }
    _background = acquired;
  }

  Future<void> _retireDevice() async {
    final owner = _owner, epoch = _epoch;
    if (owner == null || !_current(epoch, owner)) return;
    final state = _state;
    final pending = runtimeRows(state['pendingDisables']);
    final device = runtimeMap(state.remove('device'));
    state['enabled'] = false;
    if (device['node'] is String && device['device_id'] is String) {
      pending.add(device);
    }
    state['pendingDisables'] = pending;
    await context.persist();
    if (!_current(epoch, owner) || !_connected) return;
    await _drainPending();
  }

  Future<void> _drainPending() async {
    final owner = _owner, epoch = _epoch;
    if (owner == null || !_connected) return;
    final state = _state;
    final pending = runtimeRows(state['pendingDisables']);
    for (final entry in pending.toList()) {
      final disabled = await context.rpc('disable_push_device', {
        'push_service_jid': entry['service'],
        'node': entry['node'],
        'device_id': entry['device_id'],
      });
      if (!_current(epoch, owner)) return;
      if (disabled == true) {
        pending.remove(entry);
        state['pendingDisables'] = pending;
        await context.persist();
      }
    }
  }

  Future<void> handleOwnerEvent(Map<String, dynamic> event) {
    if (event['type'] == 'action' &&
        runtimeMap(event['payload'])['kind'] == 'call') {
      return _handleCallAction(event);
    }
    return _serial(() => _handle(event));
  }

  Future<void> _handle(Map<String, dynamic> event) async {
    if (_disposed) return;
    if (context.session['state'] == 'signedOut') return;
    if (_instance == null ||
        _owner == null ||
        context.session['state'] != 'ready') {
      if (_pending.length < 32) _pending.add(Map.of(event));
      return;
    }
    final epoch = _epoch, owner = _owner!;
    if (event['type'] == 'fcmTokenChanged') {
      if (_useFcm && event['instance'] == _instance && _connected) {
        await _reconcile();
      }
      return;
    }
    if ((event['type'] == 'push' ||
            event['type'] == 'endpoint' ||
            event['type'] == 'unregistered' ||
            event['type'] == 'registrationFailed') &&
        ((event['transport'] == 'fcm') != _useFcm)) {
      return;
    }
    if (event['type'] == 'endpoint') {
      if (event['instance'] != _instance ||
          !_connected ||
          context.preferences['notificationsEnabled'] == false) {
        return;
      }
      final endpoint = runtimeMap(event['endpoint']);
      final uri = Uri.tryParse('${endpoint['endpoint'] ?? ''}');
      final token = endpoint['registration_token'];
      final valid = _useFcm
          ? token is String && token.isNotEmpty && token.length <= 4096
          : uri != null && uri.scheme == 'https' && uri.host.isNotEmpty;
      if (!valid ||
          endpoint['p256dh'] is! String ||
          endpoint['auth'] is! String) {
        return;
      }
      if (!_current(epoch, owner)) return;
      final held = runtimeMap(_state['device']);
      if (jsonEncode(_state['endpoint']) != jsonEncode(endpoint)) {
        await _retireDevice();
      }
      if (!_current(epoch, owner)) return;
      if (runtimeRows(_state['pendingDisables']).isNotEmpty) return;
      final service = _state['service'];
      if (service is! String) return;
      final result =
          held.isNotEmpty &&
              jsonEncode(_state['endpoint']) == jsonEncode(endpoint)
          ? held
          : runtimeMap(
              await context.rpc('register_push_device', {
                'push_service_jid': service,
                'app_id': 'social.waddle.flutter',
                'environment': 'production',
                'credentials': {
                  'type': _useFcm ? 'fcm' : 'web_push',
                  ...endpoint,
                },
              }),
            );
      if (!_current(epoch, owner)) return;
      if (result['node'] is! String || result['device_id'] is! String) {
        _resolved = true;
        _snapshot('Push registration failed');
        return;
      }
      _state['endpoint'] = endpoint;
      _state['device'] = {...result, 'service': service};
      await context.persist();
      if (!_current(epoch, owner)) return;
      final enabled = await context.rpc('enable_push_notifications', {
        'push_service_jid': service,
        'node': result['node'],
      });
      if (!_current(epoch, owner)) return;
      _state['enabled'] = enabled == true;
      await context.persist();
      if (!_current(epoch, owner)) return;
      _resolved = true;
      await _updateBackground();
      _snapshot();
    } else if (event['type'] == 'push') {
      if (event['decrypted'] != true || event['instance'] != _instance) return;
      final envelope = NotificationEnvelope.parse(event['content']);
      if (envelope == null ||
          !_permissionGranted ||
          context.preferences['notificationsEnabled'] == false ||
          _visible ||
          _seen.contains(envelope.item)) {
        return;
      }
      if (_connected) await _delay(liveGrace);
      if (!_current(epoch, owner) ||
          _visible ||
          context.preferences['notificationsEnabled'] == false ||
          _live.contains(envelope.item)) {
        return;
      }
      await _backend.show(
        NotificationNotice(
          id: _noticeId(envelope.conversation),
          title: envelope.conversation,
          body: envelope.mention
              ? 'Mentioned you'
              : '${(envelope.unread ?? 1).clamp(1, 2147483647)} new messages',
          payload: {
            'owner': owner,
            'instance': _instance,
            ...envelope.toJson(),
          },
          sound: context.preferences['messageSoundsEnabled'] != false,
        ),
      );
      // If logout raced the platform post, clear the now-stale private banner.
      if (!_current(epoch, owner) ||
          _visible ||
          context.preferences['notificationsEnabled'] == false) {
        await _backend.clear();
        return;
      }
      _remember(_seen, envelope.item);
      context.persistent['pushAnnouncedItems'] = _seen.toList();
      await context.persist();
    } else if (event['type'] == 'action') {
      final payload = runtimeMap(event['payload']);
      if (payload['owner'] != owner || payload['instance'] != _instance) return;
      final envelope = NotificationEnvelope.parse(payload);
      if (envelope == null) return;
      if (event['action'] == 'reply') {
        final text = event['input'];
        if (text is String &&
            text.trim().isNotEmpty &&
            text.length <= 65536 &&
            reply != null) {
          var temporaryBackground = false;
          try {
            if (!_background) {
              try {
                temporaryBackground = await _backend.background(true);
                if (temporaryBackground) {
                  _background = true;
                  _snapshot();
                }
              } catch (_) {
                // No background permission dialog from a headless action. The
                // durable outbox remains the fallback when no FGS can be held.
              }
            }
            if (!_current(epoch, owner)) return;
            await reply!(
              jid: envelope.conversation,
              group: envelope.group,
              threadId: envelope.thread,
              body: text.trim(),
            );
          } catch (_) {
            // The durable outbox may already contain the reply. Do not enqueue
            // it again or echo arbitrary transport errors into a notification.
            if (_current(epoch, owner) && !_visible) {
              await _backend.show(
                NotificationNotice(
                  id: _noticeId(envelope.conversation),
                  title: envelope.conversation,
                  body: 'Open Waddle to check your reply',
                  payload: payload,
                ),
              );
              if (!_current(epoch, owner)) await _backend.clear();
            }
          } finally {
            if (temporaryBackground) {
              if (_current(epoch, owner)) {
                await _updateBackground();
                _snapshot();
              } else {
                _background = false;
                await _backend.background(false);
              }
            }
          }
        }
      } else {
        if (!_connected) {
          if (_pending.length < 32) _pending.add(event);
          return;
        }
        if (event['action'] == 'markRead') {
          await context.rpc('mark_inbox_read', {
            'partner_jid': envelope.conversation,
            'thread_id': envelope.thread,
          });
          if (!_current(epoch, owner)) return;
          await _backend.cancel(_noticeId(envelope.conversation));
          if (_current(epoch, owner)) await context.refreshHome();
          return;
        }
        if (event['action'] == 'open' || event['action'] == 'compose') {
          await _present(event);
          if (!_current(epoch, owner)) return;
        }
        _emit('navigate', {
          if (event['action'] == 'compose') 'compose': true,
          'jid': envelope.conversation,
          'title': envelope.conversation,
          'group': envelope.group,
          if (envelope.thread != null) 'threadId': envelope.thread,
        });
      }
    } else if (event['type'] == 'unregistered' ||
        event['type'] == 'registrationFailed') {
      if (event['instance'] == _instance) {
        _resolved = true;
        _snapshot('Push distributor temporarily unavailable');
      }
    }
  }

  Future<void> _present(Map<String, dynamic> event) async {
    final backend = _backend;
    if (backend is DesktopNotificationPresentation) {
      final token = event['activationToken'];
      await (backend as DesktopNotificationPresentation).present(
        token is String ? token : null,
      );
    }
  }

  Future<void> _flushPending() async {
    final events = List<Map<String, dynamic>>.of(_pending);
    _pending.clear();
    for (final event in events) {
      await _handle(event);
    }
  }

  void _remember(Set<String> set, String value) {
    set.add(value);
    if (set.length > 256) set.remove(set.first);
  }

  int _noticeId(String value) =>
      value.runes.fold(
        2166136261,
        (hash, rune) => ((hash ^ rune) * 16777619) & 0x0fffffff,
      ) |
      0x10000000;

  Future<void> dispose() async {
    if (_disposed) return;
    _disposed = true;
    _epoch++;
    _pending.clear();
    _pendingCallActions.clear();
    _callRevision++;
    await _tail;
    await _callTail.catchError((Object _) {});
    await _callNoticeTail.catchError((Object _) {});
    if (_initialized) await _backend.cancel(9003);
    if (_callForegroundSid != null) {
      await _backend.callForeground(false);
      _callForegroundSid = null;
    }
    if (_initialized && _background) {
      await _backend.background(false);
    }
    await _backend.dispose();
    await _events.close();
  }
}

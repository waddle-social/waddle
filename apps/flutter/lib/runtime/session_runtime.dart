import 'dart:async';
import 'dart:convert';
import 'dart:math';

import '../transport/transport.dart';
import 'context.dart';
import 'conversation_runtime.dart';
import 'session_auth.dart';
import 'storage.dart';
import 'surfaces_runtime.dart';

typedef PlatformInvoker =
    Future<dynamic> Function(String method, Map<String, dynamic> args);

/// An independent app owner. Neither Flutter widget lifetimes nor an Android
/// application graph own this account, its stores, or its XMPP connection.
class SessionRuntime implements RuntimeContext {
  SessionRuntime({
    required this.transportFactory,
    required this.storage,
    required this.auth,
    required PlatformInvoker platformInvoker,
    this.rawEventObserver,
    this.accountChangedObserver,
    bool initiallyVisible = true,
    Duration Function(int attempt)? reconnectDelay,
    Future<void> Function(Duration duration)? delay,
    DateTime Function()? now,
  }) : _visible = initiallyVisible,
       _platformInvoker = platformInvoker,
       _reconnectDelay = reconnectDelay ?? _backoff,
       _delay = delay ?? Future<void>.delayed,
       _now = now ?? DateTime.now {
    conversations = ConversationRuntime(this);
    surfaces = SurfaceRuntime(this);
  }
  final Future<RustTransport> Function() transportFactory;
  final RuntimeStorage storage;
  final AuthGateway auth;
  final PlatformInvoker _platformInvoker;
  final void Function(Map<String, dynamic>)? rawEventObserver;
  final void Function()? accountChangedObserver;
  final Duration Function(int) _reconnectDelay;
  final Future<void> Function(Duration) _delay;
  final DateTime Function() _now;
  late final ConversationRuntime conversations;
  late final SurfaceRuntime surfaces;
  final _events = StreamController<Map<String, dynamic>>.broadcast();
  final _snapshots = <String, Map<String, dynamic>>{};
  StreamSubscription<Map<String, dynamic>>? _transportEvents, _platformEvents;
  RustTransport? _transport, _retiringTransport;
  Future<void>? _retirement, _connecting, _disposeFuture;
  Future<void> _demandTail = Future<void>.value();
  bool _visible,
      _deliveryResolved = false,
      _pushEnabled = false,
      _backgroundDemand = false,
      _callDemand = false;
  final _demandLeases = <Object, int>{};
  Map<String, dynamic>? _credentials;
  Map<String, dynamic> _session = {'state': 'loading'};
  Map<String, dynamic> _persistent = {};
  final Map<String, dynamic> _preferences = {
    'theme': 'system',
    'timeline': 'chat',
    'wallpaperColors': false,
    'notificationsEnabled': true,
    'messageSoundsEnabled': true,
    'readReceiptsEnabled': true,
    'stayConnected': null,
  };
  final _channels = <Map<String, dynamic>>[];
  final _inbox = <String, Map<String, dynamic>>{};
  final _presence = <String, Map<String, dynamic>>{};
  final _notify = <String, String>{};
  final _joined = <String>{};
  final _joinRequests = <String, Future<dynamic>>{};
  List<Map<String, dynamic>> _providers = [];
  Future<void>? _startFuture, _restoreFuture, _refreshFuture;
  Future<void> _storageTail = Future<void>.value();
  Future<void> _lifecycleTail = Future<void>.value();
  Timer? _reconnect, _connectTimeout, _ping;
  int _generation = 0, _connectionEpoch = 0, _loginTicket = 0, _retry = 0;
  bool _closing = false,
      _disposed = false,
      _settled = false,
      _canManage = false,
      _connected = false,
      _cleanupComplete = false;
  String _resource = '';

  Stream<Map<String, dynamic>> get events => _events.stream;
  @override
  RustTransport? get transport => _transport;
  @override
  String? get jid => _session['jid'] as String?;
  @override
  int get generation => _generation;
  @override
  Map<String, dynamic> get preferences => _preferences;
  @override
  Map<String, dynamic> get session => _session;
  @override
  Map<String, dynamic> get persistent => _persistent;

  @override
  void emit(String topic, Map<String, dynamic> data) {
    if (_disposed) {
      return;
    }
    if (topic != 'navigate') {
      _snapshots[topic] = data;
    }
    _events.add({'topic': topic, 'data': data, 'generation': _generation});
  }

  void attachPlatformEvents(Stream<Map<String, dynamic>> events) {
    unawaited(_platformEvents?.cancel());
    _platformEvents = events.listen(
      (event) {
        if (_disposed) {
          return;
        }
        final eventGeneration = event['generation'];
        if (eventGeneration is int && eventGeneration != generation) {
          return;
        }
        final topic = event['topic'];
        if (topic is String) {
          final data = runtimeMap(event['data']);
          if (topic == 'delivery') {
            _deliveryResolved = data['resolved'] == true;
            _pushEnabled = data['pushEnabled'] == true;
            _backgroundDemand = data['stayConnected'] == true;
            _queueDemand();
          } else if (topic == 'call') {
            _callDemand =
                data['signaling'] == true ||
                [
                  'incoming',
                  'outgoing',
                  'connecting',
                  'active',
                ].contains(data['state']);
            _queueDemand();
          }
          emit(topic, data);
        }
      },
      onError: (_) => emit('error', {
        'message': 'A device operation could not be completed.',
      }),
    );
  }

  bool get connectionWanted =>
      _visible ||
      !_deliveryResolved ||
      !_pushEnabled ||
      _backgroundDemand ||
      _callDemand ||
      _demandLeases.containsValue(generation);

  Future<void> onVisibility(bool visible) {
    _visible = visible;
    return _applyDemand();
  }

  Future<void> effectiveBackgroundDemand(
    bool enabled, {
    bool? pushEnabled,
    bool resolved = true,
  }) {
    _backgroundDemand = enabled;
    _deliveryResolved = resolved;
    if (pushEnabled != null) {
      _pushEnabled = pushEnabled;
    }
    return _applyDemand();
  }

  void _queueDemand() {
    unawaited(
      _applyDemand().catchError((Object error) {
        if (error is AccountChanged) {
          return;
        }
        emit('error', {
          'message':
              'The previous connection could not close safely. Try reconnecting again.',
        });
      }),
    );
  }

  Future<void> _applyDemand() {
    final operation = _demandTail.catchError((Object _) {}).then((_) async {
      if (_disposed || _closing || _credentials == null) {
        return;
      }
      if (!connectionWanted) {
        if (_transport == null &&
            _connecting == null &&
            _retirement == null &&
            _retiringTransport == null &&
            _reconnect == null &&
            _connectTimeout == null) {
          return;
        }
        final captured = generation;
        await _retire();
        requireGeneration(this, captured);
        await conversations.onEvent({
          'type': 'disconnected',
          'data': <String, dynamic>{},
        });
        rawEventObserver?.call({
          'type': 'disconnected',
          'data': <String, dynamic>{},
        });
        emit('connection', {'state': 'paused'});
      } else if (_transport == null) {
        await _connect();
      }
    });
    _demandTail = operation;
    return operation;
  }

  Future<T> withConnectionDemand<T>(
    Future<T> Function() action, {
    bool waitForConnection = true,
    Duration readyTimeout = const Duration(seconds: 20),
  }) async {
    if (_disposed || _closing || _credentials == null) {
      throw StateError('Sign in first.');
    }
    final captured = generation;
    final lease = Object();
    _demandLeases[lease] = captured;
    try {
      _queueDemand();
      if (waitForConnection) {
        await _waitForConnection(captured, readyTimeout);
      }
      requireGeneration(this, captured);
      final value = await action();
      requireGeneration(this, captured);
      return value;
    } finally {
      _demandLeases.remove(lease);
      await _applyDemand();
    }
  }

  Future<void> _waitForConnection(int captured, Duration timeout) async {
    if (_connected && _transport != null) {
      return;
    }
    final done = Completer<void>();
    late final StreamSubscription<Map<String, dynamic>> subscription;
    subscription = events.listen(
      (event) {
        if (done.isCompleted) {
          return;
        }
        if (event['generation'] != captured || _disposed) {
          done.completeError(const AccountChanged());
        } else if (event['topic'] == 'connection' &&
            runtimeMap(event['data'])['state'] == 'ready') {
          done.complete();
        }
      },
      onDone: () {
        if (!done.isCompleted) {
          done.completeError(const AccountChanged());
        }
      },
    );
    try {
      await done.future.timeout(timeout);
    } finally {
      await subscription.cancel();
    }
  }

  Future<void> start() => _startFuture ??= _start();
  Future<void> _start() async {
    emit('session', _session);
    try {
      final storedPrefs = await storage.read('preferences');
      if (_disposed) {
        return;
      }
      if (storedPrefs != null) {
        _preferences.addAll(runtimeMap(jsonDecode(storedPrefs)));
      }
      _emitPreferences();
      _resource = await storage.read('resource') ?? '';
      if (_resource.isEmpty) {
        final random = Random.secure();
        _resource =
            'waddle-flutter-${List.generate(8, (_) => random.nextInt(16).toRadixString(16)).join()}';
        await storage.write('resource', _resource);
      }
      if (_disposed) {
        return;
      }
      await restore();
    } catch (_) {
      if (!_disposed) {
        _session = {
          'state': 'error',
          'message': 'Could not read the saved account. Please try again.',
        };
        emit('session', _session);
      }
    }
  }

  Future<void> restore() =>
      _restoreFuture ??= _restore().whenComplete(() => _restoreFuture = null);
  Future<void> _restore() async {
    final ticket = ++_loginTicket;
    final captured = generation;
    try {
      _session = {'state': 'loading'};
      emit('session', _session);
      final id = await storage.read('session');
      if (!_currentLogin(ticket, captured)) {
        return;
      }
      if (id == null || id.isEmpty) {
        await _signOut(revoke: false);
        return;
      }
      final value = await auth.session(id);
      if (!_currentLogin(ticket, captured)) {
        return;
      }
      if (value == null || value['is_expired'] == true) {
        await _signOut(revoke: false);
        return;
      }
      await _adopt(value);
    } catch (_) {
      if (_currentLogin(ticket, captured)) {
        _session = {
          'state': 'error',
          'message':
              'Could not restore your session. Check your connection and try again.',
        };
        emit('session', _session);
      }
    }
  }

  bool _currentLogin(int ticket, int captured) =>
      !_disposed && ticket == _loginTicket && captured == generation;

  Future<void> _adopt(Map<String, dynamic> credentials) {
    final ticket = _loginTicket;
    final captured = generation;
    final operation = _lifecycleTail.catchError((Object _) {}).then((_) async {
      if (!_currentLogin(ticket, captured)) {
        return;
      }
      try {
        await _adoptAccount(credentials, ticket, captured);
      } catch (_) {
        if (!_disposed && ticket == _loginTicket) {
          _session = {
            'state': 'error',
            'message':
                'Could not save your account securely. Please try again.',
          };
          emit('session', _session);
        }
        rethrow;
      }
    });
    _lifecycleTail = operation;
    return operation;
  }

  Future<void> _adoptAccount(
    Map<String, dynamic> credentials,
    int ticket,
    int previousGeneration,
  ) async {
    final id = credentials['session_id'];
    final owner = credentials['jid'];
    if (id is! String ||
        id.isEmpty ||
        owner is! String ||
        !owner.contains('@') ||
        credentials['xmpp_websocket_url'] is! String) {
      throw const AuthFailure();
    }
    await _retire();
    if (!_currentLogin(ticket, previousGeneration)) {
      return;
    }
    _generation++;
    final captured = generation;
    _credentials = Map.of(credentials);
    final stored = await storage.read('account:${bareJid(owner)}');
    if (_disposed || generation != captured) {
      return;
    }
    _persistent = stored == null ? {} : runtimeMap(jsonDecode(stored));
    _session = {
      'state': 'ready',
      'jid': bareJid(owner),
      'username': credentials['username'] ?? owner.split('@').first,
      'xmppLocalpart': credentials['xmpp_localpart'] ?? owner.split('@').first,
      'resource': _resource,
      'fullJid': '${bareJid(owner)}/$_resource',
      if (auth is HttpAuthGateway)
        'serverOrigin': (auth as HttpAuthGateway).baseUri.origin,
    };
    _clearAccountProjections();
    conversations.onAccountChanged();
    surfaces.onAccountChanged();
    accountChangedObserver?.call();
    await storage.write('session', id);
    if (_disposed || generation != captured) {
      return;
    }
    emit('session', _session);
    _emitPreferences();
    _emitHome();
    _queueDemand();
  }

  Future<void> _connect() {
    final pending = _connecting;
    if (pending != null) {
      return pending;
    }
    late final Future<void> operation;
    operation = _connectNow().whenComplete(() {
      if (identical(_connecting, operation)) {
        _connecting = null;
      }
    });
    _connecting = operation;
    return operation;
  }

  Future<void> _connectNow() async {
    if (_disposed || _closing || _credentials == null || !connectionWanted) {
      return;
    }
    _reconnect?.cancel();
    final captured = generation;
    final epoch = ++_connectionEpoch;
    _connected = false;
    emit('connection', {
      'state': _retry == 0 ? 'connecting' : 'reconnecting',
      'attempt': _retry,
    });
    try {
      await _transportEvents?.cancel();
      _transportEvents = null;
      await _closeTransport();
      if (_disposed || generation != captured || epoch != _connectionEpoch) {
        return;
      }
      final next = await transportFactory();
      if (_disposed || generation != captured || epoch != _connectionEpoch) {
        await _closeTransport(next);
        return;
      }
      _transport = next;
      _transportEvents = next.events.listen(
        (event) {
          if (_disposed ||
              captured != generation ||
              epoch != _connectionEpoch ||
              next != _transport) {
            return;
          }
          unawaited(
            _event(event, captured, next, epoch).catchError((Object _) {
              if (captured == generation &&
                  !_disposed &&
                  epoch == _connectionEpoch &&
                  identical(next, _transport)) {
                emit('error', {
                  'message':
                      'Could not update the conversation. Please try again.',
                });
              }
            }),
          );
        },
        onError: (_) => _scheduleReconnect(captured),
        onDone: () => _scheduleReconnect(captured),
      );
      final credential = _credentials!;
      await next.rpc('connect', {
        'server_url': credential['xmpp_websocket_url'],
        'jid': credential['jid'],
        'access_token': credential['session_id'],
        'resource': _resource,
        'resumable': false,
      });
      if (!_connected && captured == generation && epoch == _connectionEpoch) {
        _connectTimeout?.cancel();
        _connectTimeout = Timer(
          const Duration(seconds: 20),
          () => _scheduleReconnect(captured),
        );
      }
    } catch (_) {
      if (_retiringTransport != null) {
        rethrow;
      }
      _scheduleReconnect(captured);
    }
  }

  void _scheduleReconnect(int captured) {
    if (_disposed ||
        _closing ||
        captured != generation ||
        _credentials == null ||
        !connectionWanted) {
      return;
    }
    if (_reconnect?.isActive == true) {
      return;
    }
    _connectTimeout?.cancel();
    _connectTimeout = null;
    _connected = false;
    _ping?.cancel();
    final delay = _reconnectDelay(_retry++);
    emit('connection', {
      'state': 'reconnecting',
      'attempt': _retry,
      'nextDelayMs': delay.inMilliseconds,
    });
    _reconnect = Timer(delay, () {
      _reconnect = null;
      unawaited(
        _connect().catchError((Object error) {
          if (error is AccountChanged) {
            return;
          }
          emit('error', {
            'message':
                'The previous connection could not close safely. Try reconnecting again.',
          });
        }),
      );
    });
  }

  Future<void> _event(
    Map<String, dynamic> event,
    int captured,
    RustTransport source,
    int epoch,
  ) async {
    if (captured != generation ||
        _disposed ||
        !identical(source, _transport) ||
        epoch != _connectionEpoch) {
      return;
    }
    final type = event['type'];
    final raw = runtimeMap(event['data']);
    final wrapped =
        raw['message'] ?? raw['presence'] ?? raw['entry'] ?? raw['event'];
    final data = wrapped is Map ? runtimeMap(wrapped) : raw;
    final payload = event['data'];
    final normalized = <String, dynamic>{
      'type': type,
      'data': payload is List
          ? payload
          : payload is String &&
                (type == 'deliveryAcked' || type == 'deliveryFailed')
          ? {'stanza_id': payload}
          : data,
    };
    rawEventObserver?.call(normalized);
    switch (type) {
      case 'connected':
        _connected = true;
        _retry = 0;
        _reconnect?.cancel();
        _reconnect = null;
        _connectTimeout?.cancel();
        _connectTimeout = null;
        _persistent.remove('conversationUnreadCounts');
        _presence.clear();
        _joined.clear();
        _joinRequests.clear();
        emit('connection', {'state': 'ready'});
        await rpc('send_presence', {
          'status': null,
          'show': null,
          'idle_since': null,
        });
        _requireEventOwner(captured, source, epoch);
        await conversations.onEvent(normalized);
        _requireEventOwner(captured, source, epoch);
        await refreshHome();
        _requireEventOwner(captured, source, epoch);
        final joins =
            _channels
                .where((room) => room['autojoin'] != false)
                .map((room) => '${room['room_jid']}')
                .toSet()
              ..addAll(
                (_persistent['joinedRooms'] as List? ?? []).cast<String>(),
              );
        for (var index = 0; index < joins.length; index += 4) {
          await Future.wait(
            joins.skip(index).take(4).map((room) async {
              await rpc('join_room', {
                'room_jid': room,
                'nick': _session['xmppLocalpart'],
              });
              _requireEventOwner(captured, source, epoch);
              _joined.add(bareJid(room));
            }),
          );
        }
        _persistent['joinedRooms'] = _joined.toList();
        await persist();
        _requireEventOwner(captured, source, epoch);
        _emitHome();
        _ping?.cancel();
        _ping = Timer.periodic(const Duration(seconds: 180), (_) {
          if (_connected && captured == generation) {
            unawaited(
              rpc('ping_server').catchError((Object _) {
                _scheduleReconnect(captured);
                return null;
              }),
            );
          }
        });
        return;
      case 'disconnected':
        _scheduleReconnect(captured);
        break;
      case 'authenticationFailed':
        await _signOut(revoke: false);
        return;
      case 'presence':
        final from = data['from'];
        if (from is String) {
          if (data['presence_type'] == 'unavailable' ||
              data['presence_type'] == 'error') {
            _presence.remove(from);
            if ((data['muc_status_codes'] as List? ?? []).contains(110)) {
              _joined.remove(bareJid(from));
            }
          } else {
            _presence[from] = data;
          }
          _emitHome();
        }
      case 'inboxPush':
        _ingestInbox(data);
        conversations.onInboxChanged();
        _emitHome();
      case 'message':
        if (data['inbox_push'] is Map) {
          _ingestInbox(runtimeMap(data['inbox_push']));
          conversations.onInboxChanged();
        }
        _messagePreview(data);
        _emitHome();
      case 'error':
        if (data['code'] == 'resync_required') {
          _scheduleReconnect(captured);
          return;
        }
        emit('error', {
          'message': 'The connection reported an error. Please try again.',
        });
      default:
        break;
    }
    await conversations.onEvent(normalized);
    _requireEventOwner(captured, source, epoch);
    await surfaces.onEvent(normalized);
  }

  void _requireEventOwner(int captured, RustTransport source, int epoch) {
    requireGeneration(this, captured);
    if (_disposed ||
        !identical(source, _transport) ||
        epoch != _connectionEpoch) {
      throw const AccountChanged();
    }
  }

  @override
  Future<dynamic> rpc(
    String method, [
    Map<String, dynamic> args = const {},
  ]) async {
    final current = _transport;
    if (current == null || _credentials == null) {
      throw StateError('Connect to your account first.');
    }
    final captured = generation;
    if (method == 'join_room') {
      final room = bareJid('${args['room_jid'] ?? ''}');
      if (room.isEmpty) {
        throw ArgumentError('A room address is required.');
      }
      if (_joined.contains(room)) {
        return null;
      }
      final existing = _joinRequests[room];
      if (existing != null) {
        return existing;
      }
      late final Future<dynamic> operation;
      operation = _invokeTransport(current, captured, method, args)
          .then((value) {
            _joined.add(room);
            _persistent['joinedRooms'] = _joined.toList();
            unawaited(persist().catchError((Object _) {}));
            _emitHome();
            return value;
          })
          .whenComplete(() {
            if (identical(_joinRequests[room], operation)) {
              _joinRequests.remove(room);
            }
          });
      _joinRequests[room] = operation;
      return operation;
    }
    final value = await _invokeTransport(current, captured, method, args);
    if (method == 'leave_room') {
      _joined.remove(bareJid('${args['room_jid'] ?? ''}'));
      _persistent['joinedRooms'] = _joined.toList();
      await persist();
    }
    return value;
  }

  Future<dynamic> _invokeTransport(
    RustTransport current,
    int captured,
    String method,
    Map<String, dynamic> args,
  ) async {
    final result = await current.rpc(method, args);
    if (_disposed || captured != generation || current != _transport) {
      throw const AccountChanged();
    }
    return result;
  }

  @override
  Future<dynamic> platform(
    String method, [
    Map<String, dynamic> args = const {},
  ]) {
    if (method.startsWith('call.')) {
      return withConnectionDemand(
        () => _invokePlatform(method, args),
        waitForConnection: method != 'call.hangup' && method != 'call.decline',
      );
    }
    return _invokePlatform(method, args);
  }

  Future<dynamic> _invokePlatform(
    String method,
    Map<String, dynamic> args,
  ) async {
    final captured = generation;
    final result = await _platformInvoker(method, {
      ...args,
      'generation': captured,
    });
    if (_disposed || generation != captured) {
      throw const AccountChanged();
    }
    return result;
  }

  @override
  Future<void> persist() {
    final owner = jid;
    if (owner == null) {
      return Future.value();
    }
    final captured = generation;
    final encoded = jsonEncode(_persistent);
    final write = _storageTail.catchError((Object _) {}).then((_) async {
      if (!_disposed && captured == generation && jid == owner) {
        await storage.write('account:$owner', encoded);
      }
    });
    _storageTail = write;
    return write;
  }

  @override
  Future<void> refreshHome() {
    final current = _refreshFuture;
    if (current != null) {
      return current;
    }
    late final Future<void> operation;
    operation = _refreshHome().whenComplete(() {
      if (identical(_refreshFuture, operation)) {
        _refreshFuture = null;
      }
    });
    _refreshFuture = operation;
    return operation;
  }

  Future<void> _refreshHome() async {
    final captured = generation;
    final topology = runtimeMap(await rpc('discover_topology'));
    requireGeneration(this, captured);
    if (topology['channels'] is! List) {
      throw StateError('The room list could not be loaded.');
    }
    _channels
      ..clear()
      ..addAll(runtimeRows(topology['channels']));
    _settled = true;
    _emitHome();
    try {
      final inbox = runtimeMap(
        await rpc('fetch_inbox', {
          'read_filter': 'all',
          'message_content': 'include',
        }),
      );
      requireGeneration(this, captured);
      for (final entry in runtimeRows(inbox['conversations'])) {
        _ingestInbox(entry);
      }
      conversations.onInboxChanged();
      _emitHome();
    } on AccountChanged {
      rethrow;
    } catch (_) {
      emit('error', {
        'message': 'Unread messages could not be refreshed. Please try again.',
      });
    }
    try {
      _canManage = await rpc('is_community_owner') == true;
      _emitHome();
    } on AccountChanged {
      rethrow;
    } catch (_) {
      _canManage = false;
      _emitHome();
    }
    for (final method in ['fetch_user_bookmarks', 'fetch_dm_bookmarks']) {
      try {
        for (final item in runtimeRows(await rpc(method))) {
          final owner = item['room_jid'] ?? item['peer_jid'] ?? item['jid'];
          if (owner is String && item['notify_mode'] is String) {
            _notify[bareJid(owner)] = item['notify_mode'] as String;
          }
        }
      } on AccountChanged {
        rethrow;
      } catch (_) {
        /* An absent bookmark node has protocol-defined defaults. */
      }
    }
    _emitHome();
  }

  void _ingestInbox(Map<String, dynamic> entry) {
    if (entry.containsKey('thread') && !entry.containsKey('thread_id')) {
      entry = {...entry, 'thread_id': entry['thread']};
    }
    final partner = entry['partner'];
    if (partner is! String) {
      return;
    }
    final key = '${bareJid(partner)}\u0000${entry['thread_id'] ?? ''}';
    final previous = _inbox[key];
    if ((previous?['last_updated'] as num? ?? -1) >
        (entry['last_updated'] as num? ?? -1)) {
      return;
    }
    _inbox[key] = Map.of(entry);
    if (entry['thread_id'] == null) {
      _persistent['conversationUnreadCounts'] = {
        ...runtimeMap(_persistent['conversationUnreadCounts']),
        bareJid(partner): (entry['unread'] as num?)?.toInt() ?? 0,
      };
    }
    if (entry['kind'] == 'direct') {
      final peers = <String>{
        ...(_persistent['directPeers'] as List? ?? []).cast<String>(),
        bareJid(partner),
      };
      _persistent['directPeers'] = peers.toList();
      unawaited(persist().catchError((Object _) {}));
    }
  }

  void _messagePreview(Map<String, dynamic> message) {
    final from = '${message['from'] ?? ''}';
    if (from.isEmpty ||
        message['body'] is! String ||
        message['is_retracted'] == true ||
        message['replaces_id'] != null ||
        message['retracts_id'] != null) {
      return;
    }
    final room = message['message_type'] == 'groupchat';
    final partner = room || bareJid(from) != jid
        ? bareJid(from)
        : bareJid('${message['to'] ?? ''}');
    if (partner.isEmpty) {
      return;
    }
    final key = '$partner\u0000';
    final old = _inbox[key] ?? {};
    _inbox[key] = {
      ...old,
      'partner': partner,
      'kind': room ? 'muc' : 'direct',
      'preview': message['body'],
      'author': from,
      'last_updated':
          DateTime.tryParse(
            '${message['timestamp'] ?? ''}',
          )?.millisecondsSinceEpoch ??
          _now().millisecondsSinceEpoch,
    };
    if (!room) {
      _persistent['directPeers'] = <String>{
        ...(_persistent['directPeers'] as List? ?? []).cast<String>(),
        partner,
      }.toList();
      unawaited(persist().catchError((Object _) {}));
    }
  }

  void _emitHome() {
    final peopleByJid = <String, Map<String, dynamic>>{};
    final occupants = <String, List<Map<String, dynamic>>>{};
    for (final entry in _presence.entries) {
      final value = entry.value;
      final from = entry.key;
      final room = bareJid(from);
      final inRoom =
          value['muc_jid'] != null ||
          value['muc_role'] != null ||
          value['muc_affiliation'] != null;
      if (inRoom) {
        occupants.putIfAbsent(room, () => []).add(value);
      }
      final personJid = inRoom ? value['muc_jid'] as String? : from;
      if (personJid == null || !personJid.contains('@')) {
        continue;
      }
      final real = bareJid(personJid);
      final hats = runtimeRows(value['hats']);
      if (real == jid || hats.any((hat) => '${hat['uri']}'.contains('bot'))) {
        continue;
      }
      final name = inRoom ? from.split('/').last : real.split('@').first;
      final muji = runtimeMap(value['muji']);
      final live = muji.isNotEmpty && muji['preparing'] != true;
      final previous = peopleByJid[real];
      peopleByJid[real] = {
        'jid': real,
        'name': previous?['name'] ?? name,
        'show': value['show'] ?? 'available',
        'inHuddle': previous?['inHuddle'] == true || live,
        'huddleRoomJid': live ? room : previous?['huddleRoomJid'],
      };
    }
    final people = peopleByJid.values.toList()
      ..sort(
        (a, b) => '${a['name']}'.toLowerCase().compareTo(
          '${b['name']}'.toLowerCase(),
        ),
      );
    Map<String, dynamic> row(Map<String, dynamic> channel) {
      final room = bareJid('${channel['room_jid']}');
      final inbox = _inbox['$room\u0000'] ?? {};
      final members = occupants[room] ?? [];
      final huddle = members
          .where(
            (person) =>
                runtimeMap(person['muji']).isNotEmpty &&
                runtimeMap(person['muji'])['preparing'] != true,
          )
          .toList();
      return {
        'jid': room,
        'name':
            channel['bookmark_name'] ??
            channel['name'] ??
            room.split('@').first,
        'description': channel['description'],
        'groupDm': channel['is_group_dm'] == true,
        'forum': channel['channel_type'] == 'forum',
        'unread': inbox['unread'] ?? 0,
        'preview': inbox['preview'],
        'lastUpdated': inbox['last_updated'],
        'muted': _notify[room] == 'never',
        'threadUnread': _inbox.values
            .where(
              (entry) =>
                  bareJid('${entry['partner']}') == room &&
                  entry['thread_id'] != null,
            )
            .fold<int>(
              0,
              (sum, entry) => sum + ((entry['unread'] as num?)?.toInt() ?? 0),
            ),
        'here': _joined.contains(room)
            ? members
                  .where((person) => bareJid('${person['muc_jid']}') != jid)
                  .length
            : null,
        'live': huddle.isNotEmpty,
        'video': huddle.any(
          (person) => runtimeMap(person['muji'])['video'] == true,
        ),
        'huddle': huddle
            .map(
              (person) => {
                'name': '${person['from']}'.split('/').last,
                'jid': person['muc_jid'],
              },
            )
            .toList(),
      };
    }

    final dmPeers =
        (_persistent['directPeers'] as List? ?? [])
            .cast<String>()
            .where((peer) => peer != jid)
            .map((peer) {
              final inbox = _inbox['${bareJid(peer)}\u0000'] ?? {};
              return <String, dynamic>{
                'jid': peer,
                'name': peopleByJid[peer]?['name'] ?? peer.split('@').first,
                'unread': inbox['unread'] ?? 0,
                'preview': inbox['preview'],
                'lastUpdated': inbox['last_updated'],
                'show': peopleByJid[peer]?['show'],
                'muted': _notify[peer] == 'never',
              };
            })
            .toList()
          ..sort(
            (a, b) => ((b['lastUpdated'] as num?)?.toInt() ?? 0).compareTo(
              (a['lastUpdated'] as num?)?.toInt() ?? 0,
            ),
          );
    emit('home', {
      'settled': _settled,
      'canCreate': _canManage,
      'isCommunityOwner': _canManage,
      'rooms': _channels.map(row).toList(),
      'people': people,
      'dmPeers': dmPeers,
    });
    emit('activity', {
      'entries': _inbox.values
          .where((entry) => (entry['unread'] as num? ?? 0) > 0)
          .map(
            (entry) => {
              'jid': entry['partner'],
              'name': '${entry['partner']}'.split('@').first,
              'body': entry['preview'],
              'unread': entry['unread'],
              'group': entry['kind'] == 'muc',
            },
          )
          .toList(),
    });
  }

  Future<void> _loadProviders() async {
    final ticket = ++_loginTicket;
    final captured = generation;
    emit('login', {'state': 'loading'});
    try {
      final providers = await auth.providers();
      if (!_currentLogin(ticket, captured)) {
        return;
      }
      _providers = providers;
      emit('login', {
        'state': 'providers',
        'providers': providers
            .map(
              (provider) => {
                'id': provider['id'],
                'name':
                    provider['display_name'] ??
                    provider['name'] ??
                    provider['id'],
              },
            )
            .toList(),
      });
    } catch (_) {
      if (_currentLogin(ticket, captured)) {
        emit('login', {
          'state': 'failed',
          'error': 'Could not load sign-in providers. Please try again.',
        });
      }
    }
  }

  Future<void> _login(String provider) async {
    final ticket = ++_loginTicket;
    final captured = generation;
    try {
      final started = await auth.startDevice(provider);
      if (!_currentLogin(ticket, captured)) {
        return;
      }
      final code = started['device_code'];
      if (code is! String || started['user_code'] is! String) {
        throw const AuthFailure();
      }
      final interval = Duration(
        seconds: max(1, (started['interval'] as num?)?.toInt() ?? 5),
      );
      final expiry = _now().add(
        Duration(seconds: (started['expires_in'] as num?)?.toInt() ?? 600),
      );
      final uri = Uri.tryParse(
        '${started['verification_uri_complete'] ?? started['verification_uri'] ?? ''}',
      );
      if (uri == null || !['http', 'https'].contains(uri.scheme)) {
        throw const AuthFailure();
      }
      emit('login', {'state': 'verifying', 'code': started['user_code']});
      await platform('link.open', {'url': uri.toString()});
      while (_currentLogin(ticket, captured)) {
        await _delay(interval);
        if (!_currentLogin(ticket, captured)) {
          return;
        }
        if (!_now().isBefore(expiry)) {
          throw const AuthFailure();
        }
        Map<String, dynamic> polled;
        try {
          polled = await auth.pollDevice(code);
        } on AuthFailure catch (failure) {
          if (failure.transient) {
            continue;
          }
          rethrow;
        }
        if (!_currentLogin(ticket, captured)) {
          return;
        }
        if (polled['status'] != 'complete') {
          continue;
        }
        final id = polled['session_id'];
        if (id is! String || id.isEmpty) {
          throw const AuthFailure();
        }
        emit('login', {'state': 'completing'});
        final value = await auth.session(id);
        if (!_currentLogin(ticket, captured)) {
          return;
        }
        if (value == null || value['is_expired'] == true) {
          throw const AuthFailure();
        }
        await _adopt(value);
        return;
      }
    } catch (_) {
      if (_currentLogin(ticket, captured)) {
        emit('login', {
          'state': 'failed',
          'error': 'Sign-in could not be completed. Please try again.',
        });
      }
    }
  }

  Future<void> _setPreference(String key, dynamic value) async {
    const allowed = {
      'theme',
      'timeline',
      'wallpaperColors',
      'notificationsEnabled',
      'messageSoundsEnabled',
      'readReceiptsEnabled',
      'stayConnected',
    };
    if (!allowed.contains(key)) {
      throw ArgumentError('Unknown preference: $key');
    }
    if (key == 'theme' && !['system', 'light', 'dark'].contains(value) ||
        key == 'timeline' && !['chat', 'social'].contains(value)) {
      throw ArgumentError('Invalid preference value');
    }
    if (!['theme', 'timeline'].contains(key) &&
        value is! bool &&
        !(key == 'stayConnected' && value == null)) {
      throw ArgumentError('A boolean preference is required');
    }
    final previous = _preferences[key];
    _preferences[key] = value;
    try {
      await storage.write('preferences', jsonEncode(_preferences));
    } catch (_) {
      _preferences[key] = previous;
      rethrow;
    }
    _emitPreferences();
  }

  void _emitPreferences() => emit('preferences', {..._preferences});

  Future<dynamic> invoke(String method, Map<String, dynamic> args) async {
    if (_disposed || _closing) {
      throw StateError('This application runtime is closed.');
    }
    switch (method) {
      case 'ready':
        for (final entry in _snapshots.entries.toList()) {
          emit(entry.key, entry.value);
        }
        unawaited(start());
        return null;
      case 'login.providers':
        unawaited(_loadProviders());
        return null;
      case 'login.select':
        final id = args['id'];
        if (id is! String || id.isEmpty) {
          throw ArgumentError('Choose a provider.');
        }
        unawaited(_login(id));
        return null;
      case 'login.cancel':
        _loginTicket++;
        emit('login', {
          'state': 'providers',
          'providers': _providers
              .map(
                (provider) => {
                  'id': provider['id'],
                  'name': provider['display_name'] ?? provider['id'],
                },
              )
              .toList(),
        });
        return null;
      case 'session.logout':
        await _signOut();
        return null;
      case 'session.retry':
        if (_credentials == null) {
          await restore();
        } else if (_connected) {
          await refreshHome();
        } else {
          _reconnect?.cancel();
          _reconnect = null;
          await _connect();
        }
        return null;
      case 'preferences.set':
        await _setPreference('${args['key']}', args['value']);
        return null;
      case 'navigation.consume':
        _snapshots.remove('navigate');
        return null;
      case 'link.open':
        return platform(method, args);
      default:
        if (jid == null || _credentials == null) {
          throw StateError('Sign in first.');
        }
        if (method.startsWith('conversation.')) {
          return conversations.invoke(method, args);
        }
        if (method.startsWith('call.') || method.startsWith('push.')) {
          return platform(method, args);
        }
        return surfaces.invoke(method, args);
    }
  }

  void _clearAccountProjections() {
    _deliveryResolved = false;
    _pushEnabled = false;
    _backgroundDemand = false;
    _callDemand = false;
    _channels.clear();
    _inbox.clear();
    _presence.clear();
    _notify.clear();
    _joined.clear();
    _joinRequests.clear();
    _settled = false;
    _canManage = false;
    _refreshFuture = null;
    _snapshots.removeWhere(
      (key, _) => !{'preferences', 'palette', 'login'}.contains(key),
    );
  }

  Future<void> _retire() async {
    _connectionEpoch++;
    _reconnect?.cancel();
    _reconnect = null;
    _connectTimeout?.cancel();
    _connectTimeout = null;
    _ping?.cancel();
    _connected = false;
    await _transportEvents?.cancel();
    _transportEvents = null;
    await _closeTransport();
    // A factory/connect already in flight can finish after the first close.
    // Retirement does not acknowledge ownership release until that handle has
    // also crossed its transport's disconnect fence.
    await _connecting;
    await _closeTransport();
  }

  Future<void> _closeTransport([RustTransport? created]) {
    final existing = _retirement;
    if (existing != null) {
      return existing;
    }
    final previous = created ?? _retiringTransport ?? _transport;
    if (previous == null) {
      return Future<void>.value();
    }
    if (identical(previous, _transport)) {
      _transport = null;
    }
    _retiringTransport = previous;
    late final Future<void> operation;
    operation = Future<void>.sync(previous.close).then(
      (_) {
        if (identical(_retiringTransport, previous)) {
          _retiringTransport = null;
          _retirement = null;
        }
      },
      onError: (Object error, StackTrace stack) {
        // The old handle remains the retirement barrier. A later explicit retry
        // may ask it to finish closing, but can never create a second socket first.
        if (identical(_retirement, operation)) {
          _retirement = null;
        }
        Error.throwWithStackTrace(error, stack);
      },
    );
    _retirement = operation;
    return operation;
  }

  Future<void> _signOut({bool revoke = true}) async {
    final credentials = _credentials;
    final owner = jid;
    final captured = generation;
    if (_callDemand) {
      try {
        await platform('call.hangup').timeout(const Duration(seconds: 2));
      } catch (_) {
        // Capture stops immediately; signaling is best effort before logout.
      }
      requireGeneration(this, captured);
    }
    _generation++;
    _loginTicket++;
    _credentials = null;
    _persistent = {};
    _session = {'state': 'signedOut'};
    _clearAccountProjections();
    conversations.onAccountChanged();
    surfaces.onAccountChanged();
    accountChangedObserver?.call();
    emit('session', _session);
    emit('connection', {'state': 'idle'});
    _emitPreferences();
    _emitHome();
    final operation = _lifecycleTail.catchError((Object _) {}).then((_) async {
      try {
        await _retire();
      } finally {
        await _storageTail.catchError((Object _) {});
        await storage.write('session', null);
        if (owner != null) {
          await storage.write('account:$owner', null);
        }
      }
    });
    _lifecycleTail = operation;
    if (revoke && credentials?['session_id'] is String) {
      unawaited(
        auth
            .logout(credentials!['session_id'] as String)
            .catchError((Object _) {}),
      );
    }
    await operation;
  }

  Future<void> dispose() {
    final pending = _disposeFuture;
    if (pending != null) {
      return pending;
    }
    late final Future<void> operation;
    operation = _dispose().whenComplete(() {
      if (!_cleanupComplete && identical(_disposeFuture, operation)) {
        _disposeFuture = null;
      }
    });
    _disposeFuture = operation;
    return operation;
  }

  Future<void> _dispose() async {
    if (_cleanupComplete) {
      return;
    }
    _closing = true;
    _loginTicket++;
    _reconnect?.cancel();
    _ping?.cancel();
    _connectTimeout?.cancel();
    await _lifecycleTail.catchError((Object _) {});
    while (true) {
      final pending = _storageTail;
      await pending.catchError((Object _) {});
      if (identical(pending, _storageTail)) {
        break;
      }
    }
    if (!_disposed) {
      _disposed = true;
      _generation++;
      _loginTicket++;
    }
    await _retire();
    await _platformEvents?.cancel();
    conversations.dispose();
    surfaces.dispose();
    if (auth is ClosableAuthGateway) {
      (auth as ClosableAuthGateway).close();
    }
    await _events.close();
    _cleanupComplete = true;
  }

  static Duration _backoff(int attempt) => Duration(
    milliseconds:
        (min(60000, 1000 * pow(2, min(attempt, 6))) *
                (.8 + Random().nextDouble() * .4))
            .round(),
  );
}

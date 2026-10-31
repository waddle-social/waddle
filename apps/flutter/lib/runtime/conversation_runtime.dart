import 'dart:async';
import 'dart:math';
import 'dart:typed_data';

import 'context.dart';

part 'conversation_model.dart';
part 'conversation_projection.dart';

/// Account-owned conversation orchestration, shared by native Rust and WASM.
/// Widgets hold leases only; pending delivery survives navigation and reconnect.
class ConversationRuntime {
  ConversationRuntime(
    this.context, {
    this.notificationSendDeadline = const Duration(seconds: 20),
  });
  final RuntimeContext context;
  final Duration notificationSendDeadline;
  final _deliveryWaiters = <String, Completer<void>>{};
  final _views = <String, _View>{};
  final _snapshots = <String, List<Map<String, dynamic>>>{};
  final _raw = <String, List<Map<String, dynamic>>>{};
  final _outbox = <String, _Outbound>{};
  final _recent = <String, _Outbound>{};
  final _occupants = <String, Map<String, Map<String, dynamic>>>{};
  final _selfNicks = <String, String>{};
  final _pins = <String, Set<String>>{};
  final _typing = <String, Map<String, DateTime>>{};
  final _serial = <String, Future<void>>{};
  final _joins = <String, Future<void>>{};
  final _joined = <String>{};
  final _sending = <String, Object?>{};
  final _reads = <String, Future<void>>{};
  String? _active;
  Object? _storeTransport;
  Future<void>? _storeReady;
  Future<void>? _replaying;
  Object? _replayTransport;
  Timer? _typingExpiry;
  bool _connected = false;
  bool _disposed = false;
  bool? _mdsSupported;
  int _loadedGeneration = -1;

  Future<void> notificationReply({
    required String jid,
    required bool group,
    String? threadId,
    required String body,
    bool waitForConnection = false,
  }) async {
    _checkAlive();
    _loadDurable();
    final id = 'notification:${_newOriginId()}';
    final view = _View(
      id,
      bareJid(jid),
      group,
      threadId,
      context.generation,
      false,
    )..visible = false;
    _views[id] = view;
    try {
      final origin = await _enqueue(view, body);
      await _awaitNotificationDelivery(
        origin,
        waitForConnection: waitForConnection,
      );
    } finally {
      _views.remove(id);
    }
  }

  Future<void> _awaitNotificationDelivery(
    String origin, {
    required bool waitForConnection,
  }) async {
    final row = _outbox[origin];
    if (row == null || !_connected && !waitForConnection) {
      return;
    }
    if (row.failed) {
      throw StateError('Your reply could not be delivered');
    }
    final lease = _RuntimeLease(context);
    final waiter = Completer<void>();
    _deliveryWaiters[origin] = waiter;
    try {
      await waiter.future.timeout(
        notificationSendDeadline,
        onTimeout: () {
          throw StateError(
            'Your reply is queued for the next connection. It has not been confirmed as delivered.',
          );
        },
      );
      lease.check();
      await _persistOutbox();
    } finally {
      if (identical(_deliveryWaiters[origin], waiter)) {
        _deliveryWaiters.remove(origin);
      }
    }
  }

  void _completeDelivery(String origin, {Object? error}) {
    final waiter = _deliveryWaiters.remove(origin);
    if (waiter == null || waiter.isCompleted) {
      return;
    }
    if (error == null) {
      waiter.complete();
    } else {
      waiter.completeError(error);
    }
  }

  Future<dynamic> invoke(String method, Map<String, dynamic> args) async {
    _checkAlive();
    _loadDurable();
    if (method == 'conversation.open') {
      _open(args);
      return null;
    }
    final view = _view(args, allowMissing: method == 'conversation.close');
    if (view == null) {
      return null;
    }
    switch (method) {
      case 'conversation.close':
        view.typingTimer?.cancel();
        _views.remove(view.id);
        if (_active == view.id) {
          _active = null;
        }
        return null;
      case 'conversation.visibility':
        view.visible = args['visible'] == true;
        if (view.visible) {
          _active = view.id;
          _readVisible(view);
        }
        return null;
      case 'conversation.read':
        view.atNewest = args['atNewestEdge'] != false;
        _readVisible(view);
        return null;
      case 'conversation.history':
        return _history(view);
      case 'conversation.send':
        view.reply = _replyFor(view, args);
        view.edit = null;
        return _enqueue(
          view,
          _string(args['body']),
          mentions: runtimeRows(args['mentions']),
          reply: view.reply,
        );
      case 'conversation.gif':
        return _enqueue(
          view,
          _required(args, 'url'),
          reply: _replyFor(view, args),
          format: false,
          preview: false,
        );
      case 'conversation.sticker':
        return _sticker(view, args);
      case 'conversation.edit':
        return _edit(view, args);
      case 'conversation.reply':
        view.reply = _item(view, _required(args, 'targetId'));
        _emitComposer(view);
        return null;
      case 'conversation.cancelReply':
        view.reply = null;
        _emitComposer(view);
        return null;
      case 'conversation.cancelEdit':
        view.edit = null;
        _emitComposer(view);
        return null;
      case 'conversation.retry':
        return _retry(view, args);
      case 'conversation.typing':
        _onTyping(view);
        return null;
      case 'conversation.reaction':
        return _reaction(view, args);
      case 'conversation.retract':
        final item = _item(view, _required(args, 'targetId'));
        if (item['own'] != true || item['retracted'] == true) {
          throw StateError('Only your existing messages can be removed');
        }
        return _verb('send_retraction', {
          'peer_jid': view.jid,
          'target_stanza_id': _target(item),
          'audience': view.group ? 'muc' : 'direct',
        });
      case 'conversation.moderate':
        if (!view.group || !_authority(view.jid, moderate: true)) {
          throw StateError('You cannot moderate this room');
        }
        return _verb('send_moderation', {
          'room_jid': view.jid,
          'target_stanza_id': _target(_item(view, _required(args, 'targetId'))),
          'reason': args['reason'],
        });
      case 'conversation.pin':
        if (!view.group) {
          throw StateError('Only room messages can be pinned');
        }
        return _verb(
          args['pinned'] == false ? 'unpin_message' : 'pin_message',
          {
            'room_jid': view.jid,
            'target_stanza_id': _target(
              _item(view, _required(args, 'targetId')),
            ),
          },
        );
      case 'conversation.notify':
        return _notifications(view, args);
      case 'conversation.mentionSearch':
        return _mentionSearch(view, _string(args['query']));
      case 'conversation.search':
        return _search(view, _required(args, 'query'));
      case 'conversation.members':
        final rows = <String, Map<String, dynamic>>{};
        for (final affiliation in ['owner', 'admin', 'member']) {
          final result = await _rpc('list_room_members', {
            'room_jid': view.jid,
            'affiliation': affiliation,
          });
          for (final row in runtimeRows(result)) {
            final jid = _string(row['jid']);
            rows[jid] = {
              'jid': jid,
              'name': row['nick'] ?? _localName(jid),
              'nick': row['nick'],
              'affiliation': row['affiliation'] ?? affiliation,
            };
          }
        }
        return rows.values.toList();
      case 'conversation.attach':
      case 'conversation.insertContent':
      case 'conversation.pasteImage':
        return _upload(view, method, args);
      case 'conversation.fileBytes':
      case 'conversation.fileOpen':
        final file = _file(view, args);
        final lease = _RuntimeLease(context);
        final result = await context.platform(
          method == 'conversation.fileBytes' ? 'file.bytes' : 'file.open',
          {'file': file},
        );
        lease.check();
        return result;
      default:
        throw UnsupportedError('Unknown conversation command: $method');
    }
  }

  void onInboxChanged() {
    final active = _views[_active];
    if (active != null) {
      _emit(active);
    }
  }

  Future<void> onEvent(Map<String, dynamic> event) async {
    if (_disposed || context.jid == null) {
      return;
    }
    _loadDurable();
    final eventLease = _RuntimeLease(context);
    final type = event['type'];
    final data = runtimeMap(event['data']);
    switch (type) {
      case 'connected':
        _connected = true;
        _mdsSupported = null;
        _joined.clear();
        _serial.clear();
        _reads.clear();
        await _ensureStore();
        await _replay();
        if (!eventLease.currentConnection) {
          return;
        }
        final active = _views[_active];
        if (active != null) {
          active.paging = false;
          active.historyRequest = null;
          active.loading = true;
          active.cursor = null;
          active.pages = 0;
          active.reachedStart = false;
          await _initialise(active);
        }
        return;
      case 'disconnected':
        _connected = false;
        _mdsSupported = null;
        _joined.clear();
        _joins.clear();
        _reads.clear();
        for (final row in _outbox.values) {
          if (!row.acked && !row.failed) {
            row.data['status'] = 'queued';
          }
        }
        await _persistOutbox();
        return;
      case 'inboxPush':
        _emitFor(bareJid(_string(data['partner'])));
        return;
      case 'presence':
        _presence(data);
        return;
      case 'deliveryAcked':
      case 'deliveryFailed':
      case 'messageRejected':
        await _delivery(type as String, event['data']);
        return;
      case 'mdsDisplayed':
        if (event['data'] is List) {
          for (final entry in runtimeRows(event['data'])) {
            _siblingRead(entry);
          }
        } else {
          _siblingRead(data);
        }
        return;
      case 'message':
        final raw = runtimeMap(data['message'] ?? data);
        final group =
            raw['is_muc'] == true || raw['message_type'] == 'groupchat';
        if (raw['muc_user'] == true || raw['muc_pm'] == true) {
          return;
        }
        final from = _string(raw['from']);
        final jid = group
            ? bareJid(from)
            : bareJid(
                bareJid(from) == bareJid(context.jid!)
                    ? _string(raw['to'])
                    : from,
              );
        if (jid.isEmpty || jid == bareJid(context.jid!)) {
          return;
        }
        _incomingTyping(jid, raw);
        final pin = runtimeMap(raw['pin_event']);
        if (pin.isNotEmpty && from == jid && group) {
          final ids = _pins.putIfAbsent(jid, () => <String>{});
          if (pin['action'] == 'unpinned') {
            ids.remove(pin['target_stanza_id']);
          } else if (pin['target_stanza_id'] is String) {
            ids.add(pin['target_stanza_id'] as String);
          }
        }
        await _ingest(jid, group, [raw], archived: false);
        return;
    }
  }

  void onAccountChanged() {
    for (final waiter in _deliveryWaiters.values) {
      if (!waiter.isCompleted) {
        waiter.completeError(const AccountChanged());
      }
    }
    _deliveryWaiters.clear();
    for (final view in _views.values) {
      view.typingTimer?.cancel();
    }
    _typingExpiry?.cancel();
    _typingExpiry = null;
    _views.clear();
    _snapshots.clear();
    _raw.clear();
    _outbox.clear();
    _recent.clear();
    _occupants.clear();
    _selfNicks.clear();
    _pins.clear();
    _typing.clear();
    _serial.clear();
    _joined.clear();
    _joins.clear();
    _sending.clear();
    _reads.clear();
    _active = null;
    _storeTransport = null;
    _storeReady = null;
    _replaying = null;
    _replayTransport = null;
    _loadedGeneration = -1;
    _connected = false;
    _mdsSupported = null;
  }

  void dispose() {
    if (_disposed) {
      return;
    }
    onAccountChanged();
    _disposed = true;
  }

  void _checkAlive() {
    if (_disposed) {
      throw StateError('Conversation runtime is closed');
    }
    if (context.jid == null) {
      throw StateError('Sign in first');
    }
  }

  String _required(Map<String, dynamic> args, String key) =>
      _optional(args[key]) ?? (throw ArgumentError('Missing $key'));
  _View? _view(Map<String, dynamic> args, {bool allowMissing = false}) {
    final jid = bareJid(_required(args, 'jid'));
    final id = _string(args['viewId'], '$jid/${args['threadId'] ?? ''}');
    final view = _views[id];
    if (view == null && allowMissing) {
      return null;
    }
    if (view == null ||
        view.jid != jid ||
        view.generation != context.generation) {
      throw StateError('This conversation view is no longer active');
    }
    return view;
  }

  void _open(Map<String, dynamic> args) {
    final jid = bareJid(_required(args, 'jid'));
    final id = _string(args['viewId'], '$jid/${args['threadId'] ?? ''}');
    final group = args['group'] == true;
    final thread = _optional(args['threadId']);
    final existing = _views[id];
    if (existing != null &&
        (existing.jid != jid ||
            existing.group != group ||
            existing.thread != thread)) {
      throw StateError('A view lease cannot change its conversation');
    }
    final view = _views.putIfAbsent(
      id,
      () => _View(
        id,
        jid,
        group,
        thread,
        context.generation,
        args['atNewestEdge'] != false,
      ),
    );
    view.visible = true;
    view.atNewest = args['atNewestEdge'] != false;
    _active = id;
    _emit(view);
    if (existing == null) {
      _background(_initialise(view));
    } else {
      _readVisible(view);
    }
  }

  bool _valid(_View view) =>
      !_disposed &&
      view.generation == context.generation &&
      identical(_views[view.id], view);
  Future<dynamic> _rpc(
    String method, [
    Map<String, dynamic> args = const {},
  ]) async {
    final lease = _RuntimeLease(context);
    final result = await context.rpc(method, args);
    if (_disposed) {
      throw StateError('Conversation runtime is closed');
    }
    lease.check(connection: true);
    return result;
  }

  void _background(Future<void> work) {
    final lease = _RuntimeLease(context);
    unawaited(
      work.catchError((Object error) {
        if (!_disposed && lease.currentConnection && error is! AccountChanged) {
          context.emit('error', {'message': error.toString()});
        }
      }),
    );
  }

  Future<void> _verb(String method, Map<String, dynamic> args) async {
    final result = await _rpc(method, args);
    if (result != true) {
      throw StateError('The server refused $method');
    }
  }

  Future<Map<String, dynamic>> _ordered(
    String jid,
    Future<Map<String, dynamic>> Function() operation,
  ) {
    final lease = _RuntimeLease(context);
    final next = (_serial[jid] ?? Future<void>.value()).then((_) {
      lease.check(connection: true);
      return operation();
    });
    _serial[jid] = next.then<void>(
      (_) {},
      onError: (Object _, StackTrace _) {},
    );
    return next;
  }

  Future<void> _ensureStore() {
    final transport = context.transport;
    if (transport == null) {
      return Future.error(StateError('Not connected'));
    }
    if (identical(_storeTransport, transport) && _storeReady != null) {
      return _storeReady!;
    }
    _storeTransport = transport;
    _storeReady = () async {
      await _rpc('timeline_reset');
      for (final entry in _raw.entries.toList()) {
        for (final group in [false, true]) {
          final records = entry.value
              .where((row) => row['_archived'] == group)
              .toList();
          if (records.isEmpty) {
            continue;
          }
          final isGroup =
              records.first['is_muc'] == true ||
              records.first['message_type'] == 'groupchat';
          final result = await _rpc('timeline_ingest', {
            'conversation_jid': entry.key,
            'group': isGroup,
            'account_jid': context.jid,
            'own_nick': _selfNicks[entry.key],
            'messages': records,
            'archived': group,
            'complete': false,
            'cursor': null,
            'received_at_ms': DateTime.now().millisecondsSinceEpoch,
          });
          _snapshots[entry.key] = runtimeRows(runtimeMap(result)['items']);
        }
      }
    }();
    return _storeReady!;
  }

  Future<void> _initialise(_View view) async {
    if (!_valid(view)) {
      return;
    }
    final request = Object();
    view.initialiseRequest = request;
    try {
      if (!_connected || context.transport == null) {
        view.loading = false;
        _emit(view);
        return;
      }
      await _ensureStore();
      if (view.group) {
        await _ensureJoined(view.jid);
      }
      if (!_valid(view)) {
        return;
      }
      await _history(view);
      if (view.group) {
        final pins = await _rpc('fetch_room_pins', {'room_jid': view.jid});
        if (!_valid(view)) {
          return;
        }
        _pins[view.jid] = runtimeRows(pins)
            .map((row) => _string(row['target_stanza_id']))
            .where((id) => id.isNotEmpty)
            .toSet();
        _emit(view);
      }
    } finally {
      if (_valid(view) && identical(view.initialiseRequest, request)) {
        view.loading = false;
        _emit(view);
      }
    }
  }

  Future<void> _ensureJoined(String jid) async {
    if (_joined.contains(jid)) {
      return;
    }
    final existing = _joins[jid];
    if (existing != null) {
      return existing;
    }
    final work = () async {
      await _rpc('join_room', {
        'room_jid': jid,
        'nick': _string(
          context.session['xmppLocalpart'],
          _localName(context.jid!),
        ),
      });
      _joined.add(jid);
    }();
    _joins[jid] = work;
    try {
      await work;
    } finally {
      if (identical(_joins[jid], work)) {
        _joins.remove(jid);
      }
    }
  }

  Future<void> _history(_View view) async {
    if (!_valid(view) || view.paging || view.reachedStart || view.pages >= 20) {
      return;
    }
    if (!_connected) {
      throw StateError('History is unavailable while disconnected');
    }
    final request = Object();
    final lease = _RuntimeLease(context);
    view.historyRequest = request;
    view.paging = true;
    _emit(view);
    try {
      final method = view.thread != null && view.group
          ? 'fetch_room_thread_history'
          : view.group
          ? 'fetch_room_history'
          : 'fetch_dm_history';
      final page = runtimeMap(
        await _rpc(method, {
          if (view.group) 'room_jid': view.jid else 'peer_jid': view.jid,
          if (view.thread != null && view.group) 'thread_id': view.thread,
          'max_messages': 50,
          'before_id': view.cursor,
        }),
      );
      if (!_valid(view)) {
        return;
      }
      if (page['messages'] is! List) {
        throw StateError('History returned an invalid archive page');
      }
      final messages = runtimeRows(page['messages']);
      await _ingest(
        view.jid,
        view.group,
        messages,
        archived: true,
        complete: page['is_complete'] == true,
        cursor: _optional(page['first_id']),
      );
      lease.check(connection: true);
      if (!_valid(view) || !identical(view.historyRequest, request)) {
        return;
      }
      view.pages++;
      view.cursor = _optional(page['first_id']) ?? view.cursor;
      view.reachedStart = page['is_complete'] == true || messages.isEmpty;
    } finally {
      if (_valid(view) && identical(view.historyRequest, request)) {
        view.paging = false;
        view.loading = false;
        _emit(view);
        _readVisible(view);
      }
    }
  }

  Future<void> _ingest(
    String jid,
    bool group,
    List<Map<String, dynamic>> messages, {
    required bool archived,
    bool complete = false,
    String? cursor,
  }) async {
    await _ensureStore();
    final lease = _RuntimeLease(context);
    final now = DateTime.now().millisecondsSinceEpoch;
    final records = <Map<String, dynamic>>[];
    final cache = _raw.putIfAbsent(jid, () => []);
    for (final raw in messages) {
      final from = _string(raw['from']);
      final stamp = group
          ? _optional(_occupants[jid]?[from]?['muc_jid'])
          : from;
      final record = {
        ...raw,
        '_archived': archived,
        '_author_stamp': stamp,
        if (_optional(raw['timestamp']) == null)
          'timestamp': DateTime.fromMillisecondsSinceEpoch(
            now,
            isUtc: true,
          ).toIso8601String(),
      };
      records.add(record);
      cache.add(record);
    }
    if (cache.length > 1600) {
      cache.removeRange(0, cache.length - 1600);
    }
    await _ordered(jid, () async {
      final result = runtimeMap(
        await _rpc('timeline_ingest', {
          'conversation_jid': jid,
          'group': group,
          'account_jid': context.jid,
          'own_nick': _selfNicks[jid],
          'messages': records,
          'archived': archived,
          'complete': complete,
          'cursor': cursor,
          'received_at_ms': now,
        }),
      );
      lease.check(connection: true);
      _snapshots[jid] = runtimeRows(result['items']);
      _settleReflected(jid);
      _emitFor(jid);
      return result;
    });
    final active = _views[_active];
    if (active != null && active.jid == jid) {
      _readVisible(active);
    }
  }

  void _loadDurable() {
    if (_loadedGeneration == context.generation) {
      return;
    }
    _loadedGeneration = context.generation;
    _outbox.clear();
    for (final row in runtimeRows(context.persistent['conversationOutbox'])) {
      if (row['owner'] != context.jid ||
          row['origin_id'] is! String ||
          row['jid'] is! String ||
          row['body'] is! String ||
          row['timestamp_ms'] is! num) {
        continue;
      }
      _outbox[row['origin_id'] as String] = _Outbound(
        Map<String, dynamic>.from(row),
      );
    }
  }

  Future<void> _persistOutbox() async {
    _checkAlive();
    final lease = _RuntimeLease(context);
    context.persistent['conversationOutbox'] = [
      for (final row in _outbox.values)
        if (row.owner == context.jid) row.data,
    ];
    await context.persist();
    lease.check();
  }

  Map<String, dynamic>? _replyFor(_View view, Map<String, dynamic> args) {
    final id = args.containsKey('replyTo')
        ? _optional(args['replyTo'])
        : _optional(view.reply?['id']);
    return id == null ? null : Map<String, dynamic>.from(_item(view, id));
  }

  Future<String> _enqueue(
    _View view,
    String body, {
    List<Map<String, dynamic>> mentions = const [],
    Map<String, dynamic>? reply,
    List<Map<String, dynamic>> files = const [],
    Map<String, dynamic>? sticker,
    bool format = true,
    bool preview = true,
  }) async {
    if (!_valid(view)) {
      throw StateError('The conversation is no longer active');
    }
    final text = body.trim();
    if (text.isEmpty && files.isEmpty) {
      throw ArgumentError('A message must contain text or a file');
    }
    final lease = _RuntimeLease(context);
    final composed = format
        ? _compose(text, mentions)
        : _Composed(text, [], []);
    final id = _newOriginId();
    final options = _sendOptions(id);
    options['request_displayed_marker'] =
        context.preferences['readReceiptsEnabled'] != false;
    options['markup_spans'] = composed.markup;
    options['references'] = composed.references;
    options['shared_files'] = files;
    options['sticker'] = sticker;
    if (view.thread != null) {
      options['thread'] = {'id': view.thread, 'parent': null};
    }
    var wire = composed.body;
    if (reply != null) {
      final replyId = reply['id'];
      final current = (_snapshots[view.jid] ?? [])
          .where((row) => row['id'] == replyId)
          .firstOrNull;
      if (current?['retracted'] == true) {
        reply = {...reply, 'body': 'Message removed'};
      }
      final target = _target(reply);
      final raw = _rawFor(view.jid, reply);
      final author = view.group
          ? _optional(raw['from']) ?? '${view.jid}/${reply['sender']}'
          : _string(reply['author_id'], view.jid);
      options['reply'] = {'author_jid': author, 'message_id': target};
      final quoted = _string(
        reply['body'],
      ).split('\n').map((line) => '> $line').join('\n');
      final prefix = '$quoted\n\n';
      final shift = prefix.runes.length;
      options['fallback'] = {'start': 0, 'end': shift};
      options['references'] = runtimeRows(options['references'])
          .map(
            (ref) => {
              ...ref,
              'begin': (ref['begin'] as int) + shift,
              'end': (ref['end'] as int) + shift,
            },
          )
          .toList();
      options['markup_spans'] = runtimeRows(options['markup_spans'])
          .map(
            (span) => {
              ...span,
              'start': (span['start'] as int) + shift,
              'end': (span['end'] as int) + shift,
            },
          )
          .toList();
      final thread =
          _optional(reply['thread']) ?? (!view.group ? target : view.thread);
      if (thread != null) {
        options['thread'] = {'id': thread, 'parent': null};
      }
      wire = '$prefix${composed.body}';
    }
    final row = _Outbound({
      'origin_id': id,
      'owner': context.jid,
      'jid': view.jid,
      'group': view.group,
      'body': composed.body.isEmpty && files.isNotEmpty
          ? _string(files.first['name'], 'Attachment')
          : composed.body,
      'wire_body': wire,
      'options': options,
      'timestamp_ms': DateTime.now().millisecondsSinceEpoch,
      'status': 'queued',
      'acked': false,
      'failed': false,
      'preview': preview,
    });
    _outbox[id] = row;
    try {
      await _persistOutbox();
    } catch (_) {
      if (lease.current) {
        _outbox.remove(id);
      }
      rethrow;
    }
    lease.check();
    if (view.reply?['id'] == reply?['id']) {
      view.reply = null;
    }
    view.edit = null;
    _emitComposer(view);
    _emitFor(view.jid);
    if (_connected) {
      _background(_dispatch(row));
    }
    return id;
  }

  Future<void> _seedLocal(_Outbound row) async {
    await _ensureStore();
    await _ordered(row.jid, () async {
      final result = runtimeMap(
        await _rpc('timeline_local_echo', {
          'conversation_jid': row.jid,
          'group': row.group,
          'account_jid': context.jid,
          'own_nick':
              _selfNicks[row.jid] ??
              _string(
                context.session['xmppLocalpart'],
                _localName(context.jid!),
              ),
          'origin_id': row.id,
          'body': row.body,
          'timestamp_ms': row.timestamp,
          'reply_to': runtimeMap(row.options['reply']).isEmpty
              ? null
              : {
                  'id': runtimeMap(row.options['reply'])['message_id'],
                  'sender': runtimeMap(row.options['reply'])['author_jid'],
                },
          'attachments': runtimeRows(
            row.options['shared_files'],
          ).map(_localAttachment).toList(),
          'thread': row.thread,
          'parent_thread_id': runtimeMap(row.options['thread'])['parent'],
        }),
      );
      _snapshots[row.jid] = runtimeRows(result['items']);
      return result;
    });
  }

  Future<void> _dispatch(_Outbound row) async {
    if (!_connected ||
        _disposed ||
        row.owner != context.jid ||
        row.acked ||
        row.failed ||
        _sending.containsKey(row.id) &&
            identical(_sending[row.id], context.transport)) {
      return;
    }
    final lease = _RuntimeLease(context);
    _sending[row.id] = lease.transport;
    try {
      await _seedLocal(row);
      lease.check(connection: true);
      if (row.group) {
        await _ensureJoined(row.jid);
      }
      lease.check(connection: true);
      row.data['status'] = 'sending';
      _emitFor(row.jid);
      final options = Map<String, dynamic>.from(row.options);
      // Preview tokens are ephemeral and are never stored in the durable queue.
      if (row.data['preview'] == true && row.options['sticker'] == null) {
        final uri = _previewUrl(_string(row.data['wire_body']));
        if (uri != null) {
          try {
            final lookup = runtimeMap(
              await _rpc('lookup_link_preview', {
                'url': uri,
                'scope_jid': row.jid,
              }).timeout(const Duration(seconds: 2)),
            );
            final preview = runtimeMap(lookup['preview']);
            final expires = DateTime.tryParse(_string(preview['expires_at']));
            if (lookup['status'] == 'ready' &&
                expires != null &&
                expires.isAfter(DateTime.now()) &&
                preview['token'] is String) {
              options['link_preview_token'] = preview['token'];
            }
          } on TimeoutException {
            /* Sending is independent of an optional preview. */
          } catch (error) {
            if (lease.current && error is! AccountChanged) {
              context.emit('error', {
                'message':
                    'The link preview is unavailable; sending the message without it.',
              });
            }
          }
        }
      }
      lease.check(connection: true);
      final result = runtimeMap(
        await _rpc(row.group ? 'send_groupchat_message' : 'send_chat_message', {
          if (row.group) 'room_jid': row.jid else 'peer_jid': row.jid,
          'body': row.data['wire_body'],
          'options': options,
        }),
      );
      final status = _string(result['type']);
      if (status == 'sent') {
        final adopted = _optional(result['stanza_id']);
        if (adopted != null && adopted != row.id) {
          throw StateError('The transport changed the durable send identity');
        }
        row.data['status'] = row.acked ? 'acked' : 'sent';
      } else if (row.acked || row.data['reflected'] == true) {
        row.data['status'] = row.acked ? 'acked' : 'sent';
      } else if (['not_connected', 'transport_error'].contains(status)) {
        row.data['status'] = 'queued';
      } else {
        row.data['failed'] = true;
        row.data['status'] = 'failed';
        context.emit('error', {
          'message':
              'The message was not sent: ${status.isEmpty ? 'invalid transport result' : status}',
        });
      }
      await _persistOutbox();
      lease.check();
      if (row.failed) {
        _completeDelivery(
          row.id,
          error: StateError('Your reply could not be delivered'),
        );
      }
      _emitFor(row.jid);
      if (status == 'sent') {
        _typingSent(row.jid);
      }
    } catch (error) {
      if (!_disposed && lease.currentConnection) {
        row.data['status'] = row.acked ? 'acked' : 'queued';
        await _persistOutbox();
        _emitFor(row.jid);
        if (_connected && error is! AccountChanged) {
          context.emit('error', {
            'message': 'Delivery is waiting for a connection: $error',
          });
        }
      }
    } finally {
      if (identical(_sending[row.id], lease.transport)) {
        _sending.remove(row.id);
      }
    }
  }

  Future<void> _replay() {
    final transport = context.transport;
    final running = _replaying;
    if (running != null && identical(_replayTransport, transport)) {
      return running;
    }
    final work = () async {
      for (final row in _outbox.values.toList()) {
        if (!_connected || !identical(context.transport, transport)) {
          break;
        }
        if (!row.failed && !row.acked) {
          await _dispatch(row);
        }
      }
    }();
    _replaying = work;
    _replayTransport = transport;
    return work.whenComplete(() {
      if (identical(_replaying, work)) {
        _replaying = null;
      }
    });
  }

  Future<void> _delivery(String type, dynamic value) async {
    final data = runtimeMap(value);
    final id = value is String ? value : _string(data['stanza_id']);
    final row = _outbox[id] ?? _recent[id];
    if (row == null || row.owner != context.jid) {
      return;
    }
    if (type == 'messageRejected' && !_rejectionOwns(row, data)) {
      return;
    }
    if (type != 'deliveryAcked' &&
        (row.data['reflected'] == true || _hasReflection(row))) {
      return;
    }
    if (type == 'deliveryFailed' && row.acked) {
      return;
    }
    if (type == 'deliveryAcked') {
      row.data['acked'] = true;
      if (!row.failed) {
        row.data['status'] = 'acked';
      }
    } else if (type == 'messageRejected' || !row.acked) {
      row.data['failed'] = true;
      row.data['status'] = 'failed';
      row.data['rejected'] = type == 'messageRejected';
      _outbox[id] = row;
    }
    if (context.transport != null) {
      await _ordered(row.jid, () async {
        final result = runtimeMap(
          await _rpc('timeline_delivery', {
            'conversation_jid': row.jid,
            'origin_id': row.id,
            'delivery': type == 'deliveryAcked'
                ? 'sent'
                : type == 'messageRejected'
                ? 'rejected'
                : 'failed',
          }),
        );
        _snapshots[row.jid] = runtimeRows(result['items']);
        return result;
      });
    }
    _settleReflected(row.jid);
    await _persistOutbox();
    if (row.failed) {
      _completeDelivery(
        row.id,
        error: StateError('Your reply was refused by the server'),
      );
    }
    _emitFor(row.jid);
  }

  bool _hasReflection(_Outbound row) {
    return (_snapshots[row.jid] ?? []).any((item) {
      if (item['own'] != true || !_identities(item).contains(row.id)) {
        return false;
      }
      if (row.group) {
        return item['archive_id'] != null;
      }
      if (item['archive_id'] != null) {
        return true;
      }
      return (_raw[row.jid] ?? []).any(
        (raw) =>
            bareJid(_string(raw['from'])) == bareJid(row.owner) &&
            bareJid(_string(raw['to'])) == row.jid &&
            _identities(raw).contains(row.id),
      );
    });
  }

  void _settleReflected(String jid) {
    var reflectedChanged = false;
    for (final row in [..._outbox.values, ..._recent.values]) {
      if (row.jid == jid && _hasReflection(row)) {
        reflectedChanged |= row.data['reflected'] != true || row.failed;
        row.data['reflected'] = true;
        row.data['failed'] = false;
        row.data['rejected'] = false;
        row.data['status'] = row.acked ? 'acked' : 'sent';
      }
    }
    final items = _snapshots[jid] ?? const [];
    final settled = _outbox.values
        .where(
          (row) =>
              row.jid == jid &&
              row.acked &&
              !row.failed &&
              items.any(
                (item) =>
                    item['own'] == true &&
                    _identities(item).contains(row.id) &&
                    (!row.group || item['archive_id'] != null),
              ),
        )
        .toList();
    for (final row in settled) {
      _outbox.remove(row.id);
      _recent[row.id] = row;
      _completeDelivery(row.id);
    }
    while (_recent.length > 256) {
      _recent.remove(_recent.keys.first);
    }
    if (settled.isNotEmpty || reflectedChanged) {
      _background(_persistOutbox());
    }
  }

  Future<void> _retry(_View view, Map<String, dynamic> args) async {
    final key = _string(args['localId']);
    final row =
        _outbox[key] ??
        _outbox.values
            .where((row) => row.data['timestamp_ms'].toString() == key)
            .firstOrNull;
    if (row == null || row.jid != view.jid || !row.failed) {
      throw StateError('The failed message is no longer available');
    }
    // Retry retains reply, thread, attachment and Unicode annotation metadata.
    row.data['failed'] = false;
    row.data['rejected'] = false;
    row.data['acked'] = false;
    row.data['status'] = 'queued';
    await _persistOutbox();
    _emitFor(view.jid);
    if (_connected) {
      _background(_dispatch(row));
    }
  }

  Future<void> _edit(_View view, Map<String, dynamic> args) async {
    final row = _item(view, _required(args, 'targetId'));
    if (row['own'] != true || row['retracted'] == true) {
      throw StateError('Only your existing messages can be edited');
    }
    final target =
        _optional(row['correction_id']) ??
        (throw StateError('The author-assigned edit identity is unavailable'));
    view.edit = row;
    final body = args['body'];
    if (body is! String) {
      _emitComposer(view);
      return;
    }
    final composed = _compose(body.trim(), runtimeRows(args['mentions']));
    if (composed.body.isEmpty) {
      throw ArgumentError('An edit must contain text');
    }
    try {
      final result = runtimeMap(
        await _rpc('send_correction', {
          'peer_jid': view.jid,
          'target_id': target,
          'new_body': composed.body,
          'audience': view.group ? 'muc' : 'direct',
          'options': {
            ..._sendOptions(_newOriginId()),
            'markup_spans': composed.markup,
            'references': composed.references,
            'thread': _optional(row['thread']) == null
                ? null
                : {'id': row['thread'], 'parent': row['parent_thread_id']},
          },
        }),
      );
      if (!_valid(view)) {
        return;
      }
      if (result['type'] != 'sent') {
        throw StateError('The correction was not sent');
      }
    } catch (_) {
      if (_valid(view)) {
        view.editAttempt++;
        view.edit = {...row, 'body': body};
        _emitComposer(view);
      }
      rethrow;
    }
    view.edit = null;
    _emitComposer(view);
    _typingSent(view.jid);
  }

  Future<String> _sticker(_View view, Map<String, dynamic> args) async {
    final reply = _replyFor(view, args);
    final packs = runtimeRows(
      await _rpc('fetch_sticker_packs', {'owner_jid': null}),
    );
    final pack = packs
        .where((pack) => pack['id'] == args['packId'])
        .firstOrNull;
    final index = args['index'];
    final stickers = runtimeRows(pack?['stickers']);
    if (pack == null ||
        index is! int ||
        index < 0 ||
        index >= stickers.length) {
      throw StateError('The sticker is no longer available');
    }
    final sticker = stickers[index];
    if (_string(sticker['desc']).trim().isEmpty) {
      throw StateError('The sticker is missing its accessible description');
    }
    final sources = sticker['sources'] as List? ?? [];
    if (sources.isEmpty) {
      throw StateError('The sticker has no source image');
    }
    return _enqueue(
      view,
      _string(sticker['desc']),
      reply: reply,
      format: false,
      preview: false,
      sticker: {'pack_id': pack['id'], 'pack_jid': null, 'pack_node': null},
      files: [
        {
          'url': sources.first,
          'name': null,
          'media_type': sticker['media_type'],
          'size': sticker['size'],
          'width': sticker['width'],
          'height': sticker['height'],
          'hashes': sticker['hashes'] ?? [],
          'disposition': 'inline',
          'encrypted': null,
        },
      ],
    );
  }

  Future<dynamic> _upload(
    _View view,
    String method,
    Map<String, dynamic> args,
  ) async {
    final reply = _replyFor(view, args);
    if (view.upload == 'uploading') {
      throw StateError('An upload is already running');
    }
    final lease = _RuntimeLease(context);
    view.upload = 'uploading';
    _emit(view);
    try {
      final operation = method == 'conversation.attach'
          ? 'file.pickAndUpload'
          : method == 'conversation.pasteImage'
          ? 'file.pasteAndUpload'
          : args['data'] is Uint8List
          ? 'file.uploadBytes'
          : 'file.uploadUri';
      final file = runtimeMap(
        await context.platform(operation, {
          'jid': view.jid,
          'group': view.group,
          if (args['data'] != null) 'bytes': args['data'],
          'name': args['name'] ?? 'image.png',
          'media_type': args['mimeType'] ?? 'image/png',
          if (args['uri'] != null) 'uri': args['uri'],
        }),
      );
      lease.check();
      if (!_valid(view)) {
        throw StateError('The conversation was closed during upload');
      }
      if (file.isNotEmpty) {
        await _enqueue(
          view,
          '',
          files: [file],
          reply: reply,
          format: false,
          preview: false,
        );
      }
      view.upload = 'idle';
      return file.isNotEmpty;
    } catch (_) {
      if (_valid(view)) {
        view.upload = 'failed';
      }
      rethrow;
    } finally {
      if (_valid(view)) {
        _emit(view);
      }
    }
  }

  Future<void> _reaction(_View view, Map<String, dynamic> args) async {
    // Serialize the full-set replace operation, rather than calculating toggles
    // from an overlapping read of the previous reaction set.
    await _ordered(view.jid, () async {
      final row = _item(view, _required(args, 'targetId'));
      final target = _target(row);
      final emoji = _required(args, 'emoji');
      final old = runtimeRows(row['reactions'])
          .where((row) => row['own'] == true)
          .map((row) => _string(row['emoji']))
          .toSet();
      final next = {...old};
      if (!next.remove(emoji)) {
        next.add(emoji);
      }
      final accepted = await _rpc('send_reaction', {
        'target_jid': view.jid,
        'target_stanza_id': target,
        'emojis': next.toList(),
        'audience': view.group ? 'muc' : 'direct',
      });
      if (accepted != true) {
        throw StateError('The reaction was refused');
      }
      final result = runtimeMap(
        await _rpc('timeline_own_reactions', {
          'conversation_jid': view.jid,
          'target_id': target,
          'emojis': next.toList(),
          'own_jid': context.jid,
          'timestamp_ms': DateTime.now().millisecondsSinceEpoch,
        }),
      );
      _snapshots[view.jid] = runtimeRows(result['items']);
      _emitFor(view.jid);
      return result;
    });
  }

  Future<void> _notifications(_View view, Map<String, dynamic> args) async {
    final mode = _required(args, 'mode');
    if (!['always', 'on_mention', 'never'].contains(mode)) {
      throw ArgumentError('Unknown notification mode');
    }
    final result = runtimeMap(
      await _rpc(
        view.group ? 'set_room_notification_mode' : 'set_dm_notification_mode',
        {
          if (view.group) 'room_jid': view.jid else 'dm_jid': view.jid,
          'mode': mode,
          if (view.group) 'name': args['name'],
          'rich_payload_opt_in':
              context.preferences['notificationsEnabled'] != false,
        },
      ),
    );
    if (result['type'] == 'node_config_mismatch') {
      throw StateError(
        'This bookmark has incompatible notification configuration',
      );
    }
    if (!['ok', 'removed'].contains(result['type'])) {
      throw StateError('The notification preference was refused');
    }
    final modes = runtimeMap(context.persistent['notificationModes']);
    modes[view.jid] = mode;
    context.persistent['notificationModes'] = modes;
    await context.persist();
    _emitFor(view.jid);
  }

  Future<List<Map<String, dynamic>>> _search(_View view, String query) async {
    final result = runtimeMap(
      await _rpc(view.group ? 'search_room_history' : 'search_dm_history', {
        if (view.group) 'room_jid': view.jid else 'peer_jid': view.jid,
        'query': query,
        'max_messages': 50,
      }),
    );
    if (result['messages'] is! List) {
      throw StateError('Search returned an invalid archive page');
    }
    // Search is an inspection result, never a second timeline ingestion path.
    return runtimeRows(result['messages'])
        .map(
          (row) => {
            'id':
                row['mam_id'] ??
                row['stanza_id'] ??
                row['origin_id'] ??
                row['id'],
            'body': row['is_retracted'] == true
                ? 'Message removed'
                : row['body'],
            'author': view.group
                ? _resource(_string(row['from']))
                : _localName(_string(row['from'])),
            'timestamp': row['timestamp'],
          },
        )
        .toList();
  }

  Future<List<Map<String, dynamic>>> _mentionSearch(
    _View view,
    String query,
  ) async {
    if (!view.group) {
      return [];
    }
    final hits = <String, Map<String, dynamic>>{};
    for (final row in (_occupants[view.jid] ?? {}).values) {
      final jid = _optional(row['muc_jid']);
      final name = _resource(_string(row['from']));
      if (jid != null &&
          name.toLowerCase().contains(query.toLowerCase()) &&
          bareJid(jid) != bareJid(context.jid!)) {
        hits['xmpp:${bareJid(jid)}'] = {
          'jid': bareJid(jid),
          'name': name,
          'uri': 'xmpp:${bareJid(jid)}',
        };
      }
    }
    if (query.isNotEmpty) {
      final result = await _rpc('search_users', {'query': query});
      for (final row in runtimeRows(
        result is Map ? result['entries'] ?? result['users'] : result,
      )) {
        final jid = _optional(row['jid']);
        if (jid != null && bareJid(jid) != bareJid(context.jid!)) {
          hits['xmpp:${bareJid(jid)}'] = {
            'jid': bareJid(jid),
            'name': row['username'] ?? row['display_name'] ?? _localName(jid),
            'uri': 'xmpp:${bareJid(jid)}',
          };
        }
      }
    }
    return hits.values.toList();
  }

  void _presence(Map<String, dynamic> data) {
    final from = _string(data['from']);
    if (_resource(from).isEmpty) {
      return;
    }
    final jid = bareJid(from);
    final occupants = _occupants.putIfAbsent(jid, () => {});
    if (data['presence_type'] == 'unavailable' ||
        data['presence_type'] == 'error') {
      occupants.remove(from);
      if (_selfNicks[jid] == _resource(from)) {
        _selfNicks.remove(jid);
        _joined.remove(jid);
      }
    } else {
      occupants[from] = data;
      if ((data['muc_status_codes'] as List? ?? []).contains(110)) {
        _selfNicks[jid] = _resource(from);
      }
    }
    _emitFor(jid);
  }

  bool _authority(String jid, {bool moderate = false, bool owner = false}) {
    final own = (_occupants[jid] ?? {}).values
        .where((row) => (row['muc_status_codes'] as List? ?? []).contains(110))
        .firstOrNull;
    if (own == null) {
      return false;
    }
    final affiliation = own['muc_affiliation'];
    return owner
        ? affiliation == 'owner'
        : ['owner', 'admin'].contains(affiliation) ||
              moderate && own['muc_role'] == 'moderator';
  }

  void _onTyping(_View view) {
    if (!_connected || !view.visible) {
      return;
    }
    if (!view.composing) {
      view.composing = true;
      _background(_sendTyping(view, 'composing'));
    }
    view.typingTimer?.cancel();
    view.typingTimer = Timer(const Duration(seconds: 3), () {
      if (!_valid(view)) {
        return;
      }
      view.composing = false;
      if (_connected) {
        _background(_sendTyping(view, 'paused'));
      }
    });
  }

  Future<void> _sendTyping(_View view, String state) =>
      _verb('send_chat_state', {
        'peer_jid': view.jid,
        'state': state,
        'audience': view.group ? 'muc' : 'direct',
      });
  void _typingSent(String jid) {
    for (final view in _views.values.where((view) => view.jid == jid)) {
      view.typingTimer?.cancel();
      view.composing = false;
      if (_connected) {
        _background(_sendTyping(view, 'active'));
      }
    }
  }

  void _incomingTyping(String jid, Map<String, dynamic> raw) {
    final sender = _string(raw['from']);
    if (sender.isEmpty || bareJid(sender) == bareJid(context.jid!)) {
      return;
    }
    final names = _typing.putIfAbsent(jid, () => {});
    if (raw['chat_state'] == 'composing') {
      names[sender] = DateTime.now().add(const Duration(seconds: 30));
    } else if (raw['chat_state'] != null || raw['body'] != null) {
      names.remove(sender);
    }
    _typingExpiry ??= Timer.periodic(const Duration(seconds: 5), (_) {
      for (final entry in _typing.entries) {
        entry.value.removeWhere(
          (_, expires) => expires.isBefore(DateTime.now()),
        );
        _emitFor(entry.key);
      }
      if (_typing.values.every((rows) => rows.isEmpty)) {
        _typingExpiry?.cancel();
        _typingExpiry = null;
      }
    });
    _emitFor(jid);
  }

  void _readVisible(_View view) {
    if (!_valid(view) ||
        _active != view.id ||
        !view.visible ||
        !view.atNewest ||
        view.thread != null ||
        view.loading ||
        view.paging ||
        !_connected ||
        _reads.containsKey(view.jid)) {
      return;
    }
    final generation = context.generation;
    final work = _markRead(view);
    _reads[view.jid] = work.then<void>(
      (advanced) {
        if (context.generation != generation ||
            !identical(_reads[view.jid], work)) {
          return;
        }
        _reads.remove(view.jid);
        if (advanced && _valid(view)) {
          _readVisible(view);
        }
      },
      onError: (Object error, StackTrace _) {
        if (context.generation != generation ||
            !identical(_reads[view.jid], work)) {
          return;
        }
        _reads.remove(view.jid);
        if (_valid(view) && error is! AccountChanged) {
          context.emit('error', {
            'message': 'Read state could not be synchronized: $error',
          });
        }
      },
    );
  }

  Future<bool> _markRead(_View view) async {
    final items = _feedRows(view.jid);
    final targetIndex = items.lastIndexWhere(
      (row) =>
          row['own'] != true &&
          row['retracted'] != true &&
          _optional(row['reference_id']) != null,
    );
    if (targetIndex < 0) {
      return false;
    }
    final target = items[targetIndex];
    final cursorMap = runtimeMap(context.persistent['conversationReadCursors']);
    final old = runtimeMap(cursorMap[view.jid]);
    final oldId = _optional(old['id']);
    if (oldId != null) {
      final oldIndex = items.indexWhere(
        (row) => _identities(row).contains(oldId),
      );
      if (oldIndex >= targetIndex ||
          oldIndex < 0 &&
              (old['timestamp_ms'] is! num ||
                  (old['timestamp_ms'] as num) >=
                      (target['timestamp_ms'] as num))) {
        return false;
      }
    }
    final lease = _RuntimeLease(context);
    final id = _target(target);
    if (context.preferences['readReceiptsEnabled'] != false &&
        (view.group || target['marker_requested'] == true)) {
      await _verb('send_displayed', {
        'peer_jid': view.jid,
        'stanza_id': id,
        'audience': view.group ? 'muc' : 'direct',
      });
    }
    if (!_valid(view) ||
        _active != view.id ||
        !view.visible ||
        !view.atNewest) {
      return false;
    }
    if (target['archive_id'] != null && target['archive_by'] != null) {
      _mdsSupported ??= await _rpc('supports_mds_publish_options') == true;
      if (_mdsSupported == true) {
        await _verb('publish_mds_displayed', {
          'chat_jid': view.jid,
          'stanza_id': target['archive_id'],
          'stanza_id_by': target['archive_by'],
        });
      }
    }
    if (!_valid(view) ||
        _active != view.id ||
        !view.visible ||
        !view.atNewest) {
      return false;
    }
    await _rpc('mark_inbox_read', {'partner_jid': view.jid, 'thread_id': null});
    lease.check();
    final current = runtimeMap(
      runtimeMap(context.persistent['conversationReadCursors'])[view.jid],
    );
    if (current['id'] != old['id']) {
      return false;
    }
    cursorMap[view.jid] = {
      'id': id,
      'timestamp_ms': target['timestamp_ms'],
      'archive_id': target['archive_id'],
      'archive_by': target['archive_by'],
    };
    context.persistent['conversationReadCursors'] = cursorMap;
    await context.persist();
    lease.check();
    return true;
  }

  void _siblingRead(Map<String, dynamic> data) {
    final jid = bareJid(_string(data['chat_id']));
    final id = _optional(data['stanza_id']);
    if (jid.isEmpty || id == null) {
      return;
    }
    final rows = _feedRows(jid);
    final target = rows.indexWhere(
      (row) =>
          row['archive_id'] == id && row['archive_by'] == data['stanza_id_by'],
    );
    if (target < 0) {
      return;
    }
    final cursors = runtimeMap(context.persistent['conversationReadCursors']);
    final old = runtimeMap(cursors[jid]);
    final current = rows.indexWhere(
      (row) => _identities(row).contains(old['id']),
    );
    if (current >= target) {
      return;
    }
    cursors[jid] = {
      'id': rows[target]['reference_id'],
      'timestamp_ms': rows[target]['timestamp_ms'],
      'archive_id': id,
      'archive_by': data['stanza_id_by'],
    };
    context.persistent['conversationReadCursors'] = cursors;
    _background(context.persist());
    _emitFor(jid);
  }

  Map<String, dynamic> _item(_View view, String id) {
    final matches = (_snapshots[view.jid] ?? [])
        .where(
          (row) =>
              row['id'] == id ||
              row['origin_id'] == id ||
              row['reference_id'] == id,
        )
        .toList();
    if (matches.length != 1) {
      throw StateError(
        'The message is no longer available or its identity is ambiguous',
      );
    }
    return matches.single;
  }

  String _target(Map<String, dynamic> row) =>
      _optional(row['reference_id']) ??
      (throw StateError('A trusted message identity is not available'));

  Map<String, dynamic> _rawFor(String jid, Map<String, dynamic> row) {
    final ids = _identities(row);
    for (final raw in (_raw[jid] ?? []).reversed) {
      final source = _string(raw['from']);
      final actorMatches =
          _resource(source).isNotEmpty && bareJid(source) == jid
          ? _resource(source) == row['sender']
          : row['author_id'] != null && bareJid(source) == row['author_id'];
      if (actorMatches &&
          _identities(raw).intersection(ids).isNotEmpty &&
          raw['replaces_id'] == null &&
          raw['reaction_target_id'] == null &&
          raw['retracts_id'] == null) {
        return raw;
      }
    }
    if (row['own'] == true) {
      final outbound =
          _outbox[_string(row['origin_id'])] ??
          _recent[_string(row['origin_id'])];
      if (outbound != null) {
        final fallback = runtimeMap(outbound.options['fallback']);
        return {
          'from': outbound.group ? '$jid/${row['sender']}' : outbound.owner,
          '_author_stamp': outbound.owner,
          'markup_spans': outbound.options['markup_spans'],
          'reply_fallback_start': fallback['start'],
          'reply_fallback_end': fallback['end'],
          'is_sticker': outbound.options['sticker'] != null,
        };
      }
    }
    return {};
  }

  List<Map<String, dynamic>> _files(String jid, Map<String, dynamic> row) {
    final outbound =
        _outbox[_string(row['origin_id'])] ??
        _recent[_string(row['origin_id'])];
    if (outbound != null) {
      return runtimeRows(outbound.options['shared_files']);
    }
    final raw = runtimeRows(_rawFor(jid, row)['shared_files']);
    return raw.isNotEmpty
        ? raw
        : runtimeRows(row['attachments']).map(_sharedAttachment).toList();
  }

  Map<String, dynamic> _file(_View view, Map<String, dynamic> args) {
    final target = _required(args, 'targetId');
    final pending = target.startsWith('pending:')
        ? _outbox[target.substring(8)]
        : null;
    final List<Map<String, dynamic>> files;
    if (pending != null &&
        pending.jid == view.jid &&
        pending.owner == context.jid) {
      files = runtimeRows(pending.options['shared_files']);
    } else {
      final row = _item(view, target);
      if (row['retracted'] == true) {
        throw StateError('The attachment was removed');
      }
      files = _files(view.jid, row);
    }
    final rawIndex = args['fileId'];
    final index = rawIndex is int ? rawIndex : int.tryParse(_string(rawIndex));
    if (index == null || index < 0 || index >= files.length) {
      throw StateError('The attachment is no longer available');
    }
    return files[index];
  }

  List<Map<String, dynamic>> _feedRows(String jid) =>
      (_snapshots[jid] ?? []).where((row) {
        final thread = _optional(row['thread']);
        return thread == null ||
            _identities(row).contains(thread) ||
            _rawFor(jid, row)['call_thread'] != null;
      }).toList();

  void _emitFor(String jid) {
    final view = _views[_active];
    if (view != null && view.jid == jid) {
      _emit(view);
    }
  }

  void _emit(_View view) {
    if (!_valid(view) || _active != view.id) {
      return;
    }
    final stored = view.thread == null
        ? _feedRows(view.jid)
        : (_snapshots[view.jid] ?? [])
              .where(
                (row) =>
                    row['thread'] == view.thread ||
                    _identities(row).contains(view.thread),
              )
              .toList();
    final rows = <Map<String, dynamic>>[];
    for (final row in stored) {
      final pending = _outbox.values
          .where(
            (send) =>
                row['own'] == true &&
                send.jid == view.jid &&
                _identities(row).contains(send.id),
          )
          .firstOrNull;
      if (pending != null &&
          (!pending.acked ||
              pending.failed ||
              pending.group && row['archive_id'] == null)) {
        continue;
      }
      rows.add(
        _projectRow(
          row,
          _rawFor(view.jid, row),
          _files(view.jid, row),
          view.group,
          context.jid!,
          _pins[view.jid] ?? {},
        ),
      );
    }
    for (final send in _outbox.values.where(
      (row) =>
          row.jid == view.jid &&
          (view.thread == null
              ? row.thread == null
              : row.thread == view.thread),
    )) {
      rows.add(_projectPending(send));
    }
    final positions = {
      for (var index = 0; index < rows.length; index++) rows[index]: index,
    };
    rows.sort((one, two) {
      final byTime = _string(
        one['timestamp'],
      ).compareTo(_string(two['timestamp']));
      return byTime != 0 ? byTime : positions[one]!.compareTo(positions[two]!);
    });
    final cursor = runtimeMap(
      runtimeMap(context.persistent['conversationReadCursors'])[view.jid],
    );
    if (view.unreadAnchor == null && rows.isNotEmpty && cursor['id'] != null) {
      final index = stored.indexWhere(
        (row) => _identities(row).contains(cursor['id']),
      );
      final unread = index < 0
          ? null
          : stored
                .skip(index + 1)
                .where((row) => row['own'] != true && row['retracted'] != true)
                .firstOrNull;
      view.unreadAnchor = _optional(unread?['id']);
    }
    if (view.thread == null &&
        view.unreadAnchor == null &&
        cursor['id'] == null) {
      final count = runtimeMap(
        context.persistent['conversationUnreadCounts'],
      )[view.jid];
      final incoming = stored
          .where((row) => row['own'] != true && row['retracted'] != true)
          .toList();
      if (count is num && count > 0 && incoming.length >= count.toInt()) {
        view.unreadAnchor = _optional(
          incoming[incoming.length - count.toInt()]['id'],
        );
      }
    }
    for (final row in rows) {
      row['unreadDivider'] = row['id'] == view.unreadAnchor;
    }
    context.emit('conversation', {
      'jid': view.jid,
      'group': view.group,
      'threadId': view.thread,
      'viewId': view.id,
      'messages': rows,
      'typing': (_typing[view.jid] ?? {}).keys
          .map((sender) => view.group ? _resource(sender) : _localName(sender))
          .toList(),
      'loading': view.loading,
      'loadingOlder': view.paging,
      'reachedStart': view.reachedStart,
      'newestFirst': context.preferences['timeline'] == 'social',
      'pinnedIds': (_pins[view.jid] ?? {}).toList(),
      'canPin': view.group,
      'canModerate': _authority(view.jid, moderate: true),
      'canManageMembers': _authority(view.jid),
      'canConfigure': _authority(view.jid, owner: true),
      'notifyMode':
          runtimeMap(context.persistent['notificationModes'])[view.jid] ??
          'always',
      'uploadState': view.upload,
      'threads': _threadSummaries(_snapshots[view.jid] ?? []),
    });
  }

  void _emitComposer(_View view) {
    if (!_valid(view) || _active != view.id) {
      return;
    }
    final row = view.edit ?? view.reply;
    context.emit('composer', {
      'jid': view.jid,
      'viewId': view.id,
      'threadId': view.thread,
      'mode': view.edit != null
          ? 'editing'
          : view.reply != null
          ? 'replying'
          : 'normal',
      if (row != null) 'targetId': row['id'],
      if (row != null) 'body': row['body'],
      if (row != null) 'author': row['sender'],
      'attempt': view.editAttempt,
    });
  }
}

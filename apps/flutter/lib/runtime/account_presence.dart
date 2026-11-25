import 'dart:async';

/// ADR-010 account preference and this resource's XEP-0319 last interaction.
class AccountPresence {
  AccountPresence({
    required this.request,
    required this.changed,
    DateTime Function()? clock,
    this.awayAfter = const Duration(minutes: 10),
    this.extendedAfter = const Duration(minutes: 30),
  }) : _clock = clock ?? DateTime.now;
  final Future<dynamic> Function(String, Map<String, dynamic>) request;
  final void Function(Map<String, dynamic>) changed;
  final DateTime Function() _clock;
  final Duration awayAfter, extendedAfter;
  static const modes = {'automatic', 'available', 'chat', 'away', 'dnd'};
  late DateTime _active = _clock().toUtc();
  String _mode = 'automatic';
  bool _connected = false, _inCall = false;
  int _epoch = 0, _revision = 0;
  Timer? _timer;
  (String, String?)? _sent;
  Future<void> _broadcast = Future.value(), _writes = Future.value();

  (String, String?) get _current {
    if (_mode != 'automatic') return (_mode, null);
    if (_inCall) return ('available', null);
    final elapsed = _clock().toUtc().difference(_active);
    if (elapsed >= extendedAfter) return ('xa', _active.toIso8601String());
    if (elapsed >= awayAfter) return ('away', _active.toIso8601String());
    return ('available', null);
  }

  void _emit() =>
      changed({'mode': _mode, 'show': _connected ? _current.$1 : 'offline'});
  Future<void> connected() async {
    final epoch = ++_epoch;
    _connected = true;
    _sent = null;
    await refresh();
    if (_connected && epoch == _epoch) await _sync();
  }

  Future<void> refresh() async {
    final revision = _revision, epoch = _epoch;
    try {
      final value = await request('fetch_status_preference', {});
      if (_connected &&
          epoch == _epoch &&
          revision == _revision &&
          value is Map) {
        await adopt(value);
      }
    } catch (_) {
      // A reconnect can still advertise the last confirmed pick while PEP is unavailable.
    }
  }

  Future<void> adopt(Map value) async {
    if (!_connected) return;
    final mode = value['type'] == 'manual'
        ? value['status']
        : value['type'] == 'automatic'
        ? 'automatic'
        : null;
    if (mode is! String || !modes.contains(mode)) return;
    _revision++;
    if (_mode != 'automatic' && mode == 'automatic') _active = _clock().toUtc();
    _mode = mode;
    await _sync();
  }

  Future<void> select(String mode) {
    if (!modes.contains(mode)) throw ArgumentError('Unknown status');
    if (!_connected) throw StateError('Reconnect to change your status.');
    final epoch = _epoch;
    final next = _writes.then((_) async {
      if (!_connected || epoch != _epoch) {
        throw StateError('The connection changed.');
      }
      final choice = mode == 'automatic'
          ? {'type': 'automatic'}
          : {'type': 'manual', 'status': mode};
      ++_revision;
      await request('publish_status_preference', {'preference': choice});
      if (!_connected || epoch != _epoch) {
        throw StateError('The connection changed.');
      }
      await adopt(choice);
    });
    _writes = next.catchError((Object _) {});
    return next;
  }

  void interaction() {
    _active = _clock().toUtc();
    if (_connected && _mode == 'automatic' && _sent?.$1 != 'available') {
      _backgroundSync();
    }
  }

  void call(bool active) {
    if (_inCall == active) return;
    _inCall = active;
    interaction();
    if (_connected) _backgroundSync();
  }

  void _backgroundSync() {
    unawaited(_sync().catchError((Object _) {}));
  }

  Future<void> _sync() {
    final epoch = _epoch;
    final next = _broadcast.then((_) async {
      if (!_connected || epoch != _epoch) return;
      final state = _current;
      _timer?.cancel();
      if (_mode == 'automatic' && !_inCall && state.$1 != 'xa') {
        final deadline = _active.add(
          state.$1 == 'away' ? extendedAfter : awayAfter,
        );
        _timer = Timer(deadline.difference(_clock().toUtc()), _backgroundSync);
      }
      _emit();
      if (state == _sent) return;
      await request('send_presence', {
        'show': state.$1 == 'available' ? null : state.$1,
        'status': null,
        'idle_since': state.$2,
      });
      if (_connected && epoch == _epoch) _sent = state;
    });
    _broadcast = next.catchError((Object _) {});
    return next;
  }

  void disconnect() {
    _epoch++;
    _connected = false;
    _sent = null;
    _timer?.cancel();
    _timer = null;
    _broadcast = Future.value();
    _writes = Future.value();
    _emit();
  }

  void clear() {
    _mode = 'automatic';
    _inCall = false;
    _active = _clock().toUtc();
    disconnect();
  }
}

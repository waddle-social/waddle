part of 'session_runtime.dart';

const _savedAccountPrefix = 'saved-account:';

String _savedAccountKey(Uri origin, String jid) =>
    '$_savedAccountPrefix${Uri.encodeComponent(origin.origin)}/${Uri.encodeComponent(jid)}';

extension on SessionRuntime {
  Future<List<Map<String, String>>> _savedAccounts() async {
    final store = storage;
    if (store is! AccountKeyStorage || session['state'] != 'signedOut') {
      return [];
    }
    final keys = await store.keysWithPrefix(_savedAccountPrefix);
    return [
      for (final key in keys)
        if (key.substring(_savedAccountPrefix.length).split('/') case [
          final origin,
          final jid,
        ])
          {
            'origin': Uri.decodeComponent(origin),
            'jid': Uri.decodeComponent(jid),
          },
    ];
  }

  Future<void> _changeAccount(Future<void> Function() change) async {
    if (_accountTransition != null || _serverChange != null) {
      throw StateError('Another account change is in progress.');
    }
    final operation = change();
    _accountTransition = operation;
    try {
      await operation;
    } finally {
      try {
        if (!_disposed && session['state'] == 'signedOut') {
          final captured = generation;
          final saved = await _savedAccounts();
          if (!_disposed && generation == captured) {
            emit('accounts', {'saved': saved});
            unawaited(_loadProviders());
          }
        }
      } finally {
        _accountTransition = null;
      }
    }
  }

  Future<void> _parkAccount() async {
    if (storage is! AccountKeyStorage ||
        jid == null ||
        session['state'] != 'ready') {
      throw StateError('There is no active account to switch.');
    }
    await _signOut(revoke: false, park: true);
  }

  Future<void> _resumeAccount(Map<String, dynamic> args) async {
    if (storage is! AccountKeyStorage || session['state'] != 'signedOut') {
      throw StateError('Choose a saved account from the sign-in screen.');
    }
    final owner = args['jid'] as String;
    final origin = HttpAuthGateway.serverOrigin(args['origin'] as String);
    final key = _savedAccountKey(origin, owner);
    final ticket = ++_loginTicket;
    final captured = generation;
    await _lifecycleTail;
    final id = await storage.read(key);
    if (!_currentLogin(ticket, captured)) return;
    if (id == null) throw StateError('This account is no longer saved.');
    final gateway = origin == _server ? auth : authForServer?.call(origin);
    if (gateway == null) throw StateError('This server is unavailable.');
    try {
      final value = await gateway.session(id);
      if (!_currentLogin(ticket, captured)) return;
      if (value == null || value['is_expired'] == true) {
        await storage.write(key, null);
        throw StateError('This sign-in expired. Sign in again.');
      }
      if (bareJid(value['jid']?.toString() ?? '') != owner) {
        throw StateError('The server returned a different account.');
      }
      _selectAuth(origin, (_) => gateway);
      await _adopt(value);
      if (jid != owner || _disposed) return;
      await storage.write('serverOrigin', origin.toString());
      await storage.write(key, null);
    } finally {
      if (!identical(gateway, auth) && gateway is ClosableAuthGateway) {
        (gateway as ClosableAuthGateway).close();
      }
    }
  }
}

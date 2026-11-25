part of 'surfaces_runtime.dart';

class _RoomPresenceWait {
  _RoomPresenceWait(this.nick, this.join);
  final String nick;
  final bool join;
  final result = Completer<String>();
}

extension RemoteCommunitiesRuntime on SurfaceRuntime {
  void remoteDisconnected() {
    _remoteEpoch++;
    _remoteRenewed = false;
    _remoteRead = null;
    _remoteSubscriptionIds.clear();
    _remoteRenewalStatus.clear();
    _remoteRenewals.clear();
    _remoteNicks.clear();
    _remoteOffers.clear();
    for (final wait in _remotePresence.values) {
      if (!wait.result.isCompleted) {
        wait.result.completeError(
          StateError('Disconnected from the room host.'),
        );
      }
    }
    _remotePresence.clear();
  }

  void _remotePresenceEvent(Map<String, dynamic> data) {
    final from = data['from'];
    if (from is! String) return;
    final room = bareJid(from);
    final nick = from.contains('/')
        ? from.substring(from.indexOf('/') + 1)
        : '';
    final pending = _remotePresence[room];
    final type = data['presence_type'];
    if (type == 'error') {
      if (pending != null &&
          !pending.result.isCompleted &&
          (nick.isEmpty || nick == pending.nick)) {
        pending.result.completeError(
          StateError(
            'The room host refused the request: ${data['error_condition'] ?? 'presence error'}.',
          ),
        );
      }
      return;
    }
    if (nick.isEmpty ||
        !(data['muc_status_codes'] as List? ?? []).contains(110)) {
      return;
    }
    final here = type != 'unavailable';
    if (here) {
      _remoteNicks[room] = nick;
    } else {
      _remoteNicks.remove(room);
    }
    if (pending != null && !pending.result.isCompleted) {
      if (pending.join == here) {
        pending.result.complete(nick);
      } else if (pending.join) {
        pending.result.completeError(
          StateError('The host did not admit you to this room.'),
        );
      }
    }
  }

  Future<List<Map<String, dynamic>>> _savedSpaces() async {
    List<Map<String, dynamic>> rows;
    try {
      rows = await _readPrivateSpaces();
    } on AccountChanged {
      rethrow;
    } catch (_) {
      if (_remoteUnsaved.isEmpty) rethrow;
      rows = [];
    }
    final entries = {
      for (final r in rows) r['address'] as String: r,
      ..._remoteUnsaved,
    };
    return [
      for (final e in entries.entries)
        {
          ...e.value,
          if (_remoteRenewalStatus[e.key] != null)
            'status': _remoteRenewalStatus[e.key],
          if (_unfollowIntents.containsKey(e.key)) 'stopping': true,
        },
    ];
  }

  Future<dynamic> _remote(String method, Map<String, dynamic> args) async {
    if (method == 'remote.saved') {
      final saved = await _savedSpaces();
      return saved;
    }
    final address = _required(args, 'address');
    if (method == 'remote.lookup') {
      final row = runtimeMap(
        await _rpc('describe_remote_community', {'address': address}),
      );
      final key = row['address'];
      if (key is! String || key.isEmpty) {
        throw StateError('The host returned no address.');
      }
      if (_remoteOffers.length >= 256) {
        _remoteOffers.remove(_remoteOffers.keys.first);
      }
      _remoteOffers[key] = row;
      return {...row, 'joined': _remoteNicks.containsKey(key)};
    }
    if (_remoteBusy.containsKey(address)) {
      throw StateError('A request for this address is already in progress.');
    }
    final operation = Object();
    _remoteBusy[address] = operation;
    try {
      await _remoteRenewals[address];
      requireGeneration(context, _owner);
      final offer = _remoteOffers[address];
      if (offer?['kind'] == 'room') {
        return await _changeRemoteRoom(method, address, offer!);
      }
      if (offer?['kind'] != 'space' &&
          method != 'remote.retrySave' &&
          method != 'remote.forget' &&
          method != 'remote.unsubscribe') {
        throw StateError('Look up the Space before following it.');
      }
      if (method == 'remote.retrySave') {
        final pending = _remoteUnsaved[address];
        if (pending == null) {
          throw StateError('There is no pending preference.');
        }
        await _rpc('update_saved_remote_space', {
          'entry': pending,
          'forget': false,
        });
        _remoteUnsaved.remove(address);
        return {'message': 'Space preference saved.'};
      }
      final existing = (await _savedSpaces())
          .where((r) => r['address'] == address)
          .firstOrNull;
      if (method == 'remote.forget') {
        if (existing == null || existing['follow'] == true) {
          throw StateError('Unsubscribe before forgetting this Space.');
        }
        await _rpc('update_saved_remote_space', {
          'entry': existing,
          'forget': true,
        });
        _remoteUnsaved.remove(address);
        return {'message': 'Space removed from your saved list.'};
      }
      final follow = method == 'remote.follow';
      if (!follow && (method != 'remote.unsubscribe' || existing == null)) {
        throw ArgumentError('No saved subscription to remove.');
      }
      if (!follow) {
        final intent = {
          'revision': existing?['revision'],
          'subid': _remoteSubscriptionIds[address] ?? existing?['subid'],
          'hostDone': false,
          'preferenceDone': false,
        };
        final epoch = _remoteEpoch;
        await _setUnfollowIntent(address, intent);
        _requireRemoteSession(epoch);
        return {
          'message': await _finishUnfollow(address, intent, existing, epoch),
        };
      }
      final result = runtimeMap(
        await _rpc('change_remote_subscription', {
          'address': address,
          'follow': follow,
          'subid': existing?['subid'],
        }),
      );
      final state = result['state'];
      if (follow &&
          !{'subscribed', 'pending', 'unconfigured'}.contains(state)) {
        throw StateError('The host did not grant a subscription.');
      }
      final pref = {
        'address': address,
        'follow': follow,
        'subid': result['subid'],
        'name': offer?['name'] ?? existing?['name'],
        'revision': null,
      };
      if (_unfollowIntents.containsKey(address))
        await _setUnfollowIntent(address, null);
      _remoteRenewalStatus.remove(address);
      _remoteUnsaved[address] = {...pref, 'unsaved': true};
      final message = switch (state) {
        'pending' => 'The host is reviewing your subscription.',
        'unconfigured' => 'The host requires subscription configuration.',
        'subscribed' => 'Following this Space.',
        _ => 'Unsubscribed from Space updates.',
      };
      try {
        await _rpc('update_saved_remote_space', {
          'entry': pref,
          'forget': false,
        });
        _remoteUnsaved.remove(address);
      } on AccountChanged {
        rethrow;
      } catch (_) {
        return {
          'message':
              '$message Your account could not save the preference. Retry saving it.',
        };
      }
      return {'message': message};
    } finally {
      if (identical(_remoteBusy[address], operation)) {
        _remoteBusy.remove(address);
      }
    }
  }

  Future<Map<String, dynamic>> _changeRemoteRoom(
    String method,
    String address,
    Map<String, dynamic> offer,
  ) async {
    if (method == 'remote.forgetRoom' || method == 'remote.saveRoom') {
      final forget = method == 'remote.forgetRoom';
      if (forget && _remoteNicks.containsKey(address)) {
        throw StateError('Leave before forgetting this room.');
      }
      await _rpc('update_remote_room_bookmark', {
        'room_jid': address,
        'name': offer['name'],
        'autojoin': _remoteNicks.containsKey(address),
        'forget': forget,
      });
      await _refreshHome();
      return {
        'message': forget ? 'Room bookmark removed.' : 'Room bookmark saved.',
      };
    }
    final join = method == 'remote.join';
    if (!join && method != 'remote.leave') {
      throw ArgumentError('Unknown room action.');
    }
    if (join != _remoteNicks.containsKey(address)) {
      final nick = _remoteNicks[address] ?? _nick;
      final pending = _RoomPresenceWait(nick, join);
      _remotePresence[address] = pending;
      // Register the waiter before sending. A local host can echo presence before transport completion.
      final ack = pending.result.future.timeout(
        const Duration(seconds: 45),
        onTimeout: () =>
            throw StateError('The room host has not confirmed your request.'),
      );
      try {
        await Future.wait([
          ack,
          _rpc('request_remote_room_presence', {
            'room_jid': address,
            'nick': nick,
            'join': join,
          }),
        ], eagerError: true);
        requireGeneration(context, _owner);
      } finally {
        if (!pending.result.isCompleted) {
          pending.result.completeError(StateError('Room request finished.'));
        }
        if (identical(_remotePresence[address], pending)) {
          _remotePresence.remove(address);
        }
      }
    }
    final current = _remoteNicks.containsKey(address);
    if (current != join) throw StateError('The room membership changed.');
    try {
      await _rpc('update_remote_room_bookmark', {
        'room_jid': address,
        'name': join ? offer['name'] : null,
        'autojoin': join,
        'forget': false,
      });
      await _refreshHome();
    } on AccountChanged {
      rethrow;
    } catch (_) {
      return {
        'joined': current,
        'message':
            '${join ? 'Joined' : 'Left'} the room. Your account could not save the bookmark.',
        'bookmarkError': true,
      };
    }
    return {
      'joined': current,
      'message': join
          ? 'Joined the room.'
          : 'Left the room. Your bookmark is retained with autojoin off.',
    };
  }
}

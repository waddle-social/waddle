part of 'surfaces_runtime.dart';

extension RemoteSubscriptions on SurfaceRuntime {
  Map<String, dynamic> get _unfollowIntents =>
      runtimeMap(context.persistent['remoteSpaceUnfollows']);

  Future<void> _setUnfollowIntent(
    String address,
    Map<String, dynamic>? intent,
  ) async {
    final owner = _owner;
    requireGeneration(context, owner);
    final intents = _unfollowIntents;
    if (intent == null) {
      intents.remove(address);
    } else {
      intents[address] = intent;
    }
    context.persistent['remoteSpaceUnfollows'] = intents;
    await context.persist();
    requireGeneration(context, owner);
  }

  void _requireRemoteSession(int epoch) {
    requireGeneration(context, _owner);
    if (_disposed || epoch != _remoteEpoch) throw const AccountChanged();
  }

  Future<List<Map<String, dynamic>>> _readPrivateSpaces() {
    final current = _remoteRead;
    if (current != null) return current;
    final epoch = _remoteEpoch;
    late final Future<List<Map<String, dynamic>>> load;
    load = _rpc('fetch_saved_remote_spaces')
        .then((value) {
          _requireRemoteSession(epoch);
          return runtimeRows(value);
        })
        .whenComplete(() {
          if (identical(load, _remoteRead)) _remoteRead = null;
        });
    _remoteRead = load;
    return load;
  }

  Future<void> renewRemoteSubscriptions() => runZoned(() async {
    if (_remoteRenewed || _disposed) return;
    _remoteRenewed = true;
    final epoch = _remoteEpoch;
    try {
      final entries = await _readPrivateSpaces();
      _requireRemoteSession(epoch);
      final byAddress = {
        for (final row in entries) row['address'] as String: row,
      };
      final addresses = {...byAddress.keys, ..._unfollowIntents.keys}.toList();
      // Stagger federated requests to avoid flooding the home server on login.
      for (var start = 0; start < addresses.length; start += 4) {
        _requireRemoteSession(epoch);
        await Future.wait(
          addresses.skip(start).take(4).map((address) async {
            if (_remoteBusy.containsKey(address)) return;
            late final Future<void> job;
            job = _renewRemoteSpace(address, byAddress[address], epoch)
                .whenComplete(() {
                  if (identical(_remoteRenewals[address], job))
                    _remoteRenewals.remove(address);
                });
            _remoteRenewals[address] = job;
            await job;
          }),
        );
      }
    } catch (_) {
      if (epoch == _remoteEpoch) _remoteRenewed = false;
    }
  }, zoneValues: {SurfaceRuntime._commandOwner: context.generation});

  Future<void> _renewRemoteSpace(
    String address,
    Map<String, dynamic>? entry,
    int epoch,
  ) async {
    try {
      final intent = runtimeMap(_unfollowIntents[address]);
      if (intent.isNotEmpty) {
        final revision = entry?['revision'];
        final followedAgain =
            entry?['follow'] == true &&
            revision is String &&
            intent['revision'] is String &&
            revision != intent['revision'];
        if (!followedAgain) {
          await _finishUnfollow(address, intent, entry, epoch);
          return;
        }
        await _setUnfollowIntent(address, null);
      }
      _requireRemoteSession(epoch);
      if (entry?['follow'] != true ||
          _remoteUnsaved[address]?['follow'] == false ||
          _remoteBusy.containsKey(address))
        return;
      final response = runtimeMap(
        await _rpc('change_remote_subscription', {
          'address': address,
          'follow': true,
          'subid': entry?['subid'],
        }),
      );
      _requireRemoteSession(epoch);
      final state = response['state'];
      if (!{'subscribed', 'unconfigured', 'pending'}.contains(state)) {
        throw StateError('The host did not renew this subscription.');
      }
      if (response['subid'] case final String id)
        _remoteSubscriptionIds[address] = id;
      _remoteRenewalStatus[address] = switch (state) {
        'pending' => 'Awaiting host approval',
        'unconfigured' => 'The host requires subscription configuration',
        _ => 'Following',
      };
    } on AccountChanged {
      rethrow;
    } catch (error) {
      _requireRemoteSession(epoch);
      _remoteRenewalStatus[address] = 'Updates unavailable: $error';
    }
  }

  /// A stopped subscription may require two independent acknowledgements. Store both
  /// outcomes so reconnecting cannot renew updates the user explicitly stopped.
  Future<String> _finishUnfollow(
    String address,
    Map<String, dynamic> intent,
    Map<String, dynamic>? entry,
    int epoch,
  ) async {
    final next = {...intent};
    if (next['hostDone'] != true) {
      try {
        await _rpc('change_remote_subscription', {
          'address': address,
          'follow': false,
          'subid': next['subid'],
        });
        _requireRemoteSession(epoch);
        next['hostDone'] = true;
        _remoteSubscriptionIds.remove(address);
      } on AccountChanged {
        rethrow;
      } catch (_) {
        _requireRemoteSession(epoch);
      }
    }
    if (entry == null || entry['follow'] != true) next['preferenceDone'] = true;
    if (next['preferenceDone'] != true) {
      try {
        await _rpc('update_saved_remote_space', {
          'entry': {
            'address': address,
            'name': entry?['name'],
            'follow': false,
            'revision': null,
            'subid': null,
          },
          'forget': false,
        });
        _requireRemoteSession(epoch);
        next['preferenceDone'] = true;
      } on AccountChanged {
        rethrow;
      } catch (_) {
        _requireRemoteSession(epoch);
      }
    }
    _requireRemoteSession(epoch);
    final done = next['hostDone'] == true && next['preferenceDone'] == true;
    await _setUnfollowIntent(address, done ? null : next);
    _remoteUnsaved.remove(address);
    final status = done
        ? 'Unsubscribed from Space updates.'
        : next['hostDone'] == true
        ? 'Unsubscribed. Your other devices may still renew updates until your account saves the preference.'
        : 'Updates are turned off on this device. The host has not confirmed the unsubscribe.';
    _remoteRenewalStatus[address] = status;
    return status;
  }
}

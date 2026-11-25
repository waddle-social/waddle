import 'dart:async';
import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_flutter/runtime/surfaces_runtime.dart';
import 'platform_test_context.dart';

const enabled = 'xmpp:spaces.remote.test?;node=on';
const disabled = 'xmpp:spaces.remote.test?;node=off';
Map<String, dynamic> preference(String address, bool follow, String revision) =>
    {
      'address': address,
      'follow': follow,
      'revision': revision,
      'name': null,
      'subid': 'old',
    };

void main() {
  test(
    'login renews only opted-in accounts once and uses the latest host subid to stop',
    () async {
      final ctx = TestRuntimeContext();
      final surface = SurfaceRuntime(ctx);
      addTearDown(surface.dispose);
      final prefs = [
        preference(enabled, true, 'v1'),
        preference(disabled, false, 'v3'),
      ];
      ctx.handler = (method, args) async {
        if (method == 'fetch_saved_remote_spaces') return prefs;
        if (method == 'change_remote_subscription')
          return {
            'state': args['follow'] == true ? 'subscribed' : 'none',
            'subid': 'new-host-id',
          };
        return null;
      };
      await Future.wait([
        surface.renewRemoteSubscriptions(),
        surface.renewRemoteSubscriptions(),
      ]);
      final changes = ctx.calls.where(
        (c) => c['method'] == 'change_remote_subscription',
      );
      expect(changes, hasLength(1));
      expect(changes.single['args']['address'], enabled);
      await surface.invoke('remote.unsubscribe', {'address': enabled});
      expect(changes.last['args'], containsPair('subid', 'new-host-id'));
      expect(changes.last['args']['follow'], isFalse);
      expect(
        ctx.calls.where((c) => c['method'] == 'request_remote_room_presence'),
        isEmpty,
      );
    },
  );

  test(
    'partial unfollow survives restart and a later explicit re-follow on a different resource supersedes it',
    () async {
      final ctx = TestRuntimeContext();
      var stored = preference(enabled, true, 'rev-1');
      var offline = true;
      ctx.handler = (method, args) async {
        if (method == 'fetch_saved_remote_spaces') return [stored];
        if (method == 'change_remote_subscription') {
          if (offline) throw StateError('host unavailable');
          return {
            'state': args['follow'] == true ? 'subscribed' : 'none',
            'subid': 'new',
          };
        }
        if (method == 'update_saved_remote_space')
          throw StateError('private PEP offline');
        return null;
      };
      var surface = SurfaceRuntime(ctx);
      final result = await surface.invoke('remote.unsubscribe', {
        'address': enabled,
      });
      expect(result['message'], contains('has not confirmed'));
      expect(ctx.persistent['remoteSpaceUnfollows'], contains(enabled));
      surface.dispose();
      surface = SurfaceRuntime(ctx);
      await surface.renewRemoteSubscriptions();
      final calls = ctx.calls.where(
        (c) => c['method'] == 'change_remote_subscription',
      );
      expect(calls.every((c) => c['args']['follow'] == false), isTrue);
      surface.dispose();
      stored = preference(enabled, true, 'other-resource-explicit-follow');
      offline = false;
      surface = SurfaceRuntime(ctx);
      await surface.renewRemoteSubscriptions();
      expect(calls.last['args']['follow'], isTrue);
      expect(ctx.persistent['remoteSpaceUnfollows'], isEmpty);
      surface.dispose();
    },
  );

  test(
    'a previous connection cannot subscribe or write after account replacement',
    () async {
      final ctx = TestRuntimeContext();
      final read = Completer<dynamic>();
      final first = Completer<void>();
      ctx.handler = (method, args) async {
        if (method == 'fetch_saved_remote_spaces') {
          first.complete();
          return read.future;
        }
        throw StateError('unexpected $method');
      };
      final surface = SurfaceRuntime(ctx);
      addTearDown(surface.dispose);
      final renewal = surface.renewRemoteSubscriptions();
      await first.future;
      ctx.generation++;
      ctx.jid = 'bob@home.test';
      ctx.persistent = {};
      surface.onAccountChanged();
      read.complete([preference(enabled, true, 'a')]);
      await renewal;
      expect(ctx.calls.map((c) => c['method']), ['fetch_saved_remote_spaces']);
      expect(ctx.persistent, isEmpty);
    },
  );
}

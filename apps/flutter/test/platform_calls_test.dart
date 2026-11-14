import 'dart:async';
import 'dart:convert';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_flutter/runtime/context.dart';
import 'package:waddle_flutter/runtime/platform_calls.dart';
import 'package:waddle_flutter/runtime/platform_media.dart';
import 'platform_test_context.dart';

class FakeMedia extends CallMedia {
  int connects = 0, disconnects = 0;
  @override
  Map<String, dynamic> get snapshot => {
    'mediaState': 'idle',
    'microphoneEnabled': false,
    'cameraEnabled': false,
  };
  @override
  Future<void> connect(
    Map<String, dynamic> join,
    bool video,
    List<Map<String, dynamic>> iceServices,
  ) async {
    connects++;
  }

  @override
  Future<void> disconnect() async {
    disconnects++;
  }

  @override
  Future<void> microphone(bool enabled) async {}
  @override
  Future<void> camera(bool enabled) async {}
  @override
  Future<void> speaker(bool enabled) async {}
  @override
  Future<void> flipCamera() async {}
  @override
  Widget buildVideo(BuildContext context, {bool local = false}) =>
      const SizedBox();
}

Map<String, dynamic> proposal(String sid) => {
  'type': 'call',
  'data': {
    'from': 'bob@example.test/phone',
    'sid': sid,
    'kind': {
      'type': 'propose',
      'media': {'audio': true, 'video': true},
    },
  },
};
Map<String, dynamic> fixtureGrant([String room = 'fixture']) {
  String token(Map<String, dynamic> payload) =>
      'hdr.${base64Url.encode(utf8.encode(jsonEncode(payload))).replaceAll('=', '')}.sig';
  return {
    'url': 'wss://media.example.test',
    'room': room,
    'identity': 'alice@example.test/flutter-1',
    'token': token({
      'video': {'roomJoin': true, 'room': room},
      'sub': 'alice@example.test/flutter-1',
    }),
  };
}

Map<String, dynamic> grantEvent(String sid) => {
  'type': 'call',
  'data': {
    'from': 'bob@example.test/phone',
    'sid': sid,
    'kind': 'session-initiate',
    'join': fixtureGrant(),
  },
};
Future<void> settle() => Future<void>.delayed(Duration.zero);

void main() {
  test('answer permission delay cannot accept a successor ring', () async {
    final context = TestRuntimeContext();
    final permission = Completer<void>();
    final media = FakeMedia();
    final service = PlatformCallService(
      context,
      media: media,
      requestCapture: (_) => permission.future,
    );
    final views = <Map<String, dynamic>>[];
    final sub = service.events.listen(
      (event) => views.add(runtimeMap(event['data'])),
    );
    service.onEvent(proposal('first'));
    await settle();
    final answer = service.invoke('call.accept', {'expectedSid': 'first'});
    final failed = expectLater(answer, throwsStateError);
    service.onEvent({
      'type': 'call',
      'data': {
        'from': 'bob@example.test/phone',
        'sid': 'first',
        'kind': 'retract',
      },
    });
    await settle();
    service.onEvent(proposal('second'));
    await settle();
    permission.complete();
    await failed;
    expect(
      context.calls.where((call) => call['method'] == 'send_call_proceed'),
      isEmpty,
    );
    expect(views.last['state'], 'incoming');
    expect(views.last['sid'], 'second');
    await sub.cancel();
    await service.dispose();
  });

  test(
    'stale call controls cannot end or change the successor active call',
    () async {
      final context = TestRuntimeContext(), media = FakeMedia();
      final service = PlatformCallService(
        context,
        media: media,
        requestCapture: (_) async {},
      );
      final views = <Map<String, dynamic>>[];
      final subscription = service.events.listen(
        (event) => views.add(runtimeMap(event['data'])),
      );
      service.onEvent(proposal('first'));
      await settle();
      service.onEvent({
        'type': 'call',
        'data': {
          'from': 'bob@example.test/phone',
          'sid': 'first',
          'kind': 'retract',
        },
      });
      await settle();
      service.onEvent(proposal('second'));
      await settle();
      await service.invoke('call.accept', {'expectedSid': 'second'});
      service.onEvent(grantEvent('second'));
      await settle();
      await settle();
      expect(views.last['state'], 'active');
      final callsBefore = context.calls.length,
          disconnectsBefore = media.disconnects;
      for (final method in [
        'call.hangup',
        'call.microphone',
        'call.camera',
        'call.speaker',
        'call.flipCamera',
        'call.hand',
        'call.dismiss',
        'call.accept',
        'call.decline',
      ]) {
        await expectLater(
          service.invoke(method, {'expectedSid': 'first'}),
          throwsStateError,
        );
      }
      expect(context.calls.length, callsBefore);
      expect(media.disconnects, disconnectsBefore);
      expect(views.last['state'], 'active');
      expect(views.last['sid'], 'second');
      // Internal teardown without an expected SID intentionally addresses the current call.
      await service.invoke('call.hangup', {});
      expect(views.last['state'], 'ended');
      await subscription.cancel();
      await service.dispose();
    },
  );

  test('account change during capture permission emits no proceed', () async {
    final context = TestRuntimeContext();
    final permission = Completer<void>();
    final foreground = <bool>[];
    final service = PlatformCallService(
      context,
      media: FakeMedia(),
      requestCapture: (_) => permission.future,
      callForeground: (active, video, sid) async {
        foreground.add(active);
      },
    );
    service.onEvent(proposal('first'));
    await settle();
    final answer = service.invoke('call.accept', {'sid': 'first'});
    final failed = expectLater(answer, throwsA(isA<AccountChanged>()));
    context.generation++;
    context.jid = 'eve@example.test';
    service.onAccountChanged();
    permission.complete();
    await failed;
    expect(foreground, isEmpty);
    expect(
      context.calls.where((call) => call['method'] == 'send_call_proceed'),
      isEmpty,
    );
    await service.dispose();
  });

  test('foreign or stale session acceptance cannot open media', () async {
    final context = TestRuntimeContext();
    final media = FakeMedia();
    final service = PlatformCallService(
      context,
      media: media,
      requestCapture: (_) async {},
    );
    final views = <Map<String, dynamic>>[];
    final sub = service.events.listen(
      (event) => views.add(runtimeMap(event['data'])),
    );
    await service.invoke('call.start', {
      'jid': 'bob@example.test',
      'video': false,
    });
    final sid = views.last['sid'];
    service.onEvent({
      'type': 'call',
      'data': {
        'from': 'mallory@example.test/phone',
        'sid': sid,
        'kind': 'session-accept',
        'join': {},
      },
    });
    service.onEvent({
      'type': 'call',
      'data': {
        'from': 'bob@example.test/phone',
        'sid': 'old',
        'kind': 'session-accept',
        'join': {},
      },
    });
    await settle();
    expect(media.connects, 0);
    await service.invoke('call.hangup', {});
    await sub.cancel();
    await service.dispose();
  });

  test(
    'a cancelled outgoing request cannot reactivate after its RPC finishes',
    () async {
      final context = TestRuntimeContext();
      final propose = Completer<dynamic>();
      context.handler = (method, args) async =>
          method == 'send_call_propose' ? propose.future : true;
      final service = PlatformCallService(
        context,
        media: FakeMedia(),
        requestCapture: (_) async {},
      );
      final views = <Map<String, dynamic>>[];
      final sub = service.events.listen(
        (event) => views.add(runtimeMap(event['data'])),
      );
      final start = service.invoke('call.start', {'jid': 'bob@example.test'});
      final failed = expectLater(start, throwsStateError);
      await settle();
      await service.invoke('call.hangup', {});
      propose.complete(true);
      await failed;
      expect(views.last['state'], 'ended');
      await sub.cancel();
      await service.dispose();
    },
  );

  test(
    'timeout stops capture immediately while retaining signaling demand',
    () async {
      final context = TestRuntimeContext();
      final media = FakeMedia();
      final teardown = Completer<dynamic>(), entered = Completer<void>();
      context.handler = (method, args) async {
        if (method == 'send_call_retract') {
          entered.complete();
          return await teardown.future;
        }
        return true;
      };
      final service = PlatformCallService(
        context,
        media: media,
        requestCapture: (_) async {},
        setupTimeout: const Duration(milliseconds: 5),
      );
      final views = <Map<String, dynamic>>[];
      final subscription = service.events.listen(
        (event) => views.add(runtimeMap(event['data'])),
      );
      await service.invoke('call.start', {'jid': 'bob@example.test'});
      await entered.future.timeout(const Duration(seconds: 1));
      expect(media.disconnects, greaterThan(0));
      expect(views.last['state'], 'ended');
      expect(views.last['signaling'], true);
      teardown.complete(true);
      await settle();
      expect(views.last['signaling'], false);
      await subscription.cancel();
      await service.dispose();
    },
  );

  test(
    'retained room teardown prevents resuming that same cached call',
    () async {
      final context = TestRuntimeContext();
      context.persistent['callSessions'] = {
        'room@muc.example.test': {
          'sid': 'retained',
          'self_full_jid': 'alice@example.test/flutter-1',
          'terminate_pending': true,
        },
      };
      final terminate = Completer<dynamic>(), entered = Completer<void>();
      context.handler = (method, args) async {
        if (method == 'send_muji_session_terminate') {
          entered.complete();
          return await terminate.future;
        }
        return true;
      };
      final service = PlatformCallService(
        context,
        media: FakeMedia(),
        requestCapture: (_) async {},
      );
      final leaving = service.invoke('call.leaveRetained', {
        'jid': 'room@muc.example.test',
      });
      await entered.future;
      await expectLater(
        service.invoke('call.start', {
          'jid': 'room@muc.example.test',
          'group': true,
        }),
        throwsStateError,
      );
      expect(
        context.calls.where(
          (call) => call['method'] == 'send_muji_session_initiate',
        ),
        isEmpty,
      );
      terminate.complete(true);
      await leaving;
      expect(runtimeMap(context.persistent['callSessions']), isEmpty);
      await service.dispose();
    },
  );

  test(
    'retired termination retry never deletes a successor cached room session',
    () async {
      final context = TestRuntimeContext();
      const room = 'room@muc.example.test';
      context.persistent['callSessions'] = {
        room: {'sid': 'retired', 'terminate_pending': true},
      };
      final terminate = Completer<dynamic>(), entered = Completer<void>();
      context.handler = (method, args) async {
        if (method == 'send_muji_session_terminate') {
          entered.complete();
          return await terminate.future;
        }
        return true;
      };
      final service = PlatformCallService(
        context,
        media: FakeMedia(),
        requestCapture: (_) async {},
      );
      service.onEvent({'type': 'connected'});
      await entered.future;
      context.persistent['callSessions'] = {
        room: {'sid': 'successor', 'terminate_pending': false},
      };
      terminate.complete(true);
      await settle();
      expect(
        runtimeMap(runtimeMap(context.persistent['callSessions'])[room])['sid'],
        'successor',
      );
      await service.dispose();
    },
  );

  test(
    'hangup during media preparation cannot start a new LiveKit room afterward',
    () async {
      final media = LiveKitCallMedia();
      final join = fixtureGrant();
      final starting = media.connect(join, true, []);
      await media.disconnect();
      await starting;
      expect(media.snapshot['mediaState'], 'idle');
      media.dispose();
    },
  );

  test(
    'call foreground starts after permission and superseded startup never opens media',
    () async {
      final context = TestRuntimeContext(), media = FakeMedia();
      final permission = Completer<void>(),
          foreground = Completer<void>(),
          entered = Completer<void>();
      final hooks = <bool>[];
      final service = PlatformCallService(
        context,
        media: media,
        requestCapture: (_) => permission.future,
        callForeground: (active, video, sid) async {
          hooks.add(active);
          if (active) {
            entered.complete();
            await foreground.future;
          }
        },
      );
      service.onEvent(proposal('ring'));
      await settle();
      final answer = service.invoke('call.accept', {'sid': 'ring'});
      await settle();
      expect(hooks, isEmpty);
      final failed = expectLater(answer, throwsA(isA<AccountChanged>()));
      permission.complete();
      await entered.future;
      expect(media.connects, 0);
      context.generation++;
      service.onAccountChanged();
      foreground.complete();
      await failed;
      await settle();
      expect(hooks, [true, false]);
      expect(media.connects, 0);
      await service.dispose();
    },
  );

  test(
    'call foreground remains until local media and signaling teardown finish',
    () async {
      final context = TestRuntimeContext(), media = FakeMedia();
      final finish = Completer<dynamic>(), entered = Completer<void>();
      context.handler = (method, args) async {
        if (method == 'send_call_finish') {
          entered.complete();
          return await finish.future;
        }
        return true;
      };
      final hooks = <bool>[];
      final service = PlatformCallService(
        context,
        media: media,
        requestCapture: (_) async {},
        callForeground: (active, video, sid) async {
          hooks.add(active);
        },
      );
      service.onEvent(proposal('ring'));
      await settle();
      await service.invoke('call.accept', {'sid': 'ring'});
      service.onEvent(grantEvent('ring'));
      await settle();
      await settle();
      expect(media.connects, 1);
      expect(hooks, [true]);
      final hangingUp = service.invoke('call.hangup', {});
      await entered.future;
      expect(media.disconnects, greaterThan(0));
      expect(hooks, [true]);
      finish.complete(true);
      await hangingUp;
      expect(hooks, [true, false]);
      await service.dispose();
    },
  );

  test(
    'late foreground cleanup cannot stop the successor call service',
    () async {
      final context = TestRuntimeContext(), media = FakeMedia();
      final finish = Completer<dynamic>(), entered = Completer<void>();
      context.handler = (method, args) async {
        if (method == 'send_call_finish' && args['sid'] == 'first') {
          entered.complete();
          return await finish.future;
        }
        return true;
      };
      final hooks = <Map<String, dynamic>>[];
      final service = PlatformCallService(
        context,
        media: media,
        requestCapture: (_) async {},
        callForeground: (active, video, sid) async {
          hooks.add({'active': active, 'sid': sid});
        },
      );
      service.onEvent(proposal('first'));
      await settle();
      await service.invoke('call.accept', {'sid': 'first'});
      service.onEvent(grantEvent('first'));
      await settle();
      await settle();
      final hangingUp = service.invoke('call.hangup', {});
      await entered.future;
      service.onEvent(proposal('second'));
      await settle();
      await service.invoke('call.accept', {'sid': 'second'});
      service.onEvent(grantEvent('second'));
      await settle();
      await settle();
      expect(hooks.map((entry) => entry['active']), [true, true]);
      expect(hooks[0]['sid'], isNot(hooks[1]['sid']));
      finish.complete(true);
      await hangingUp;
      expect(hooks.map((entry) => entry['active']), [true, true]);
      await service.invoke('call.hangup', {});
      expect(hooks.map((entry) => entry['active']), [true, true, false]);
      expect(hooks.last['sid'], hooks[1]['sid']);
      await service.dispose();
    },
  );

  test(
    'foreground acquisition failure prevents proceed and media capture',
    () async {
      final context = TestRuntimeContext(), media = FakeMedia();
      final hooks = <bool>[], views = <Map<String, dynamic>>[];
      final service = PlatformCallService(
        context,
        media: media,
        requestCapture: (_) async {},
        callForeground: (active, video, sid) async {
          hooks.add(active);
          if (active) {
            throw StateError('Foreground service refused');
          }
        },
      );
      final subscription = service.events.listen(
        (event) => views.add(runtimeMap(event['data'])),
      );
      service.onEvent(proposal('ring'));
      await settle();
      await expectLater(
        service.invoke('call.accept', {'sid': 'ring'}),
        throwsStateError,
      );
      await settle();
      await settle();
      expect(media.connects, 0);
      expect(hooks, [true, false]);
      expect(views.last['state'], 'incoming');
      expect(views.last['accepting'], false);
      expect(
        context.calls.where((call) => call['method'] == 'send_call_proceed'),
        isEmpty,
      );
      await subscription.cancel();
      await service.dispose();
    },
  );

  test('failed cached room resume retires its actual retained sid', () async {
    final context = TestRuntimeContext();
    context.persistent['callSessions'] = {
      'room@muc.example.test': {
        'sid': 'cached-sid',
        'self_full_jid': 'alice@example.test/flutter-1',
        'terminate_pending': false,
        'join': fixtureGrant('room@muc.example.test'),
      },
    };
    context.handler = (method, args) async {
      if (method == 'update_muji_presence' && args['active'] == true) {
        throw StateError('The active presence failed');
      }
      return true;
    };
    final views = <Map<String, dynamic>>[];
    final service = PlatformCallService(
      context,
      media: FakeMedia(),
      requestCapture: (_) async {},
    );
    final subscription = service.events.listen(
      (event) => views.add(runtimeMap(event['data'])),
    );
    await expectLater(
      service.invoke('call.start', {
        'jid': 'room@muc.example.test',
        'group': true,
      }),
      throwsStateError,
    );
    expect(views.last['state'], 'ended');
    expect(
      context.calls
          .where((call) => call['method'] == 'send_muji_session_terminate')
          .single['args'],
      {'room_jid': 'room@muc.example.test', 'sid': 'cached-sid'},
    );
    await subscription.cancel();
    await service.dispose();
  });

  test(
    'call foreground precedes paused signaling and is released on setup failure',
    () async {
      for (final incoming in [false, true]) {
        final context = TestRuntimeContext(), media = FakeMedia();
        final network = Completer<dynamic>(), entered = Completer<void>();
        final hooks = <bool>[], sequence = <String>[];
        final method = incoming ? 'send_call_proceed' : 'send_call_propose';
        context.handler = (name, args) async {
          if (name == method) {
            sequence.add('network');
            entered.complete();
            return await network.future;
          }
          return true;
        };
        final service = PlatformCallService(
          context,
          media: media,
          requestCapture: (_) async {
            sequence.add('permission');
          },
          callForeground: (active, video, sid) async {
            hooks.add(active);
            if (active) {
              sequence.add('foreground');
            }
          },
        );
        if (incoming) {
          service.onEvent(proposal('ring'));
          await settle();
          expect(hooks, isEmpty);
        }
        final operation = service.invoke(
          incoming ? 'call.accept' : 'call.start',
          incoming ? {'expectedSid': 'ring'} : {'jid': 'bob@example.test'},
        );
        final failed = expectLater(operation, throwsStateError);
        await entered.future;
        expect(sequence, ['permission', 'foreground', 'network']);
        expect(hooks, [true]);
        expect(media.connects, 0);
        network.completeError(StateError('Signaling failed'));
        await failed;
        expect(hooks, [true, false]);
        expect(media.connects, 0);
        await service.dispose();
      }
    },
  );

  test(
    'failed proceed retires media if a fast grant already arrived',
    () async {
      final context = TestRuntimeContext(), media = FakeMedia();
      final proceed = Completer<dynamic>(), entered = Completer<void>();
      context.handler = (method, args) async {
        if (method == 'send_call_proceed') {
          entered.complete();
          return await proceed.future;
        }
        return true;
      };
      final hooks = <bool>[], views = <Map<String, dynamic>>[];
      final service = PlatformCallService(
        context,
        media: media,
        requestCapture: (_) async {},
        callForeground: (active, video, sid) async {
          hooks.add(active);
        },
      );
      final subscription = service.events.listen(
        (event) => views.add(runtimeMap(event['data'])),
      );
      service.onEvent(proposal('ring'));
      await settle();
      final answer = service.invoke('call.accept', {'expectedSid': 'ring'});
      final failed = expectLater(answer, throwsStateError);
      await entered.future;
      service.onEvent(grantEvent('ring'));
      await settle();
      await settle();
      expect(media.connects, 1);
      expect(hooks, [true]);
      proceed.completeError(StateError('Proceed failed'));
      await failed;
      expect(views.last['state'], 'ended');
      expect(hooks, [true, false]);
      expect(media.disconnects, greaterThan(0));
      await subscription.cancel();
      await service.dispose();
    },
  );

  test(
    'grant validation rejects identity/room mismatch and insecure media URLs',
    () {
      String token(Map<String, dynamic> payload) =>
          'hdr.${base64Url.encode(utf8.encode(jsonEncode(payload))).replaceAll('=', '')}.sig';
      final join = {
        'url': 'wss://media.example.test',
        'room': 'dm1',
        'identity': 'alice@example.test/flutter-1',
        'token': token({
          'video': {'roomJoin': true, 'room': 'dm1'},
          'sub': 'alice@example.test/flutter-1',
        }),
      };
      expect(() => validateMediaGrant(join), returnsNormally);
      expect(
        () => validateMediaGrant({...join, 'room': 'dm2'}),
        throwsStateError,
      );
      expect(
        () => validateMediaGrant({
          ...join,
          'identity': 'other@example.test/phone',
        }),
        throwsStateError,
      );
      expect(
        () => validateMediaGrant({...join, 'url': 'ws://media.example.test'}),
        throwsStateError,
      );
    },
  );
}

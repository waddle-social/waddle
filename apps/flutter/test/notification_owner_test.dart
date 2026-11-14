import 'dart:async';
import 'dart:isolate';
import 'dart:ui';

import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_flutter/runtime/notification_owner.dart';

const portName = 'social.waddle.flutter.account-owner';

void main() {
  tearDown(() {
    NotificationOwner.release();
    IsolateNameServer.removePortNameMapping(portName);
  });

  test('a headless engine cannot create a second account owner', () async {
    final existing = ReceivePort();
    addTearDown(existing.close);
    expect(
      IsolateNameServer.registerPortWithName(existing.sendPort, portName),
      isTrue,
    );
    final owns = await NotificationOwner.acquire(
      headless: true,
      onEvent: (_) async {},
      onRetire: () async {},
    );
    expect(owns, isFalse);
    expect(IsolateNameServer.lookupPortByName(portName), existing.sendPort);
  });

  test(
    'UI takeover waits for acknowledged account drain before receiving events',
    () async {
      final old = ReceivePort();
      addTearDown(old.close);
      final drain = Completer<void>();
      var retired = false;
      expect(
        IsolateNameServer.registerPortWithName(old.sendPort, portName),
        isTrue,
      );
      old.listen((dynamic raw) async {
        if (raw is! Map || raw['type'] != 'takeover') return;
        await drain.future;
        retired = true;
        IsolateNameServer.removePortNameMapping(portName);
        (raw['reply'] as SendPort).send(true);
      });
      final received = <Map<String, dynamic>>[];
      var acquired = false;
      final acquiring = NotificationOwner.acquire(
        headless: false,
        onEvent: (event) async {
          expect(retired, isTrue);
          received.add(event);
        },
        onRetire: () async {},
      ).then((value) => acquired = value);
      await Future<void>.delayed(Duration.zero);
      expect(acquired, isFalse);
      drain.complete();
      expect(await acquiring, isTrue);
      expect(
        await NotificationOwner.deliver({
          'type': 'action',
          'input': 'queued reply',
        }),
        isTrue,
      );
      expect(received.single['input'], 'queued reply');
    },
  );

  test(
    'inline reply arriving during retirement transfers once to the successor',
    () async {
      final old = ReceivePort(), successor = ReceivePort();
      addTearDown(old.close);
      addTearDown(successor.close);
      expect(
        IsolateNameServer.registerPortWithName(old.sendPort, portName),
        isTrue,
      );
      old.listen((dynamic raw) {
        (raw['reply'] as SendPort).send('retiring');
      });
      final received = <Map<String, dynamic>>[];
      successor.listen((dynamic raw) {
        received.add(Map<String, dynamic>.from(raw['event'] as Map));
        (raw['reply'] as SendPort).send(true);
      });
      final timer = Timer(const Duration(milliseconds: 60), () {
        IsolateNameServer.removePortNameMapping(portName);
        IsolateNameServer.registerPortWithName(successor.sendPort, portName);
      });
      addTearDown(timer.cancel);
      expect(
        await NotificationOwner.deliver({
          'type': 'action',
          'action': 'reply',
          'input': 'retain me',
        }),
        isTrue,
      );
      expect(received, hasLength(1));
      expect(received.single['input'], 'retain me');
    },
  );

  test('an executed owner failure never retries an inline reply', () async {
    final owner = ReceivePort();
    addTearDown(owner.close);
    expect(
      IsolateNameServer.registerPortWithName(owner.sendPort, portName),
      isTrue,
    );
    var calls = 0;
    owner.listen((dynamic raw) {
      calls++;
      (raw['reply'] as SendPort).send(false);
    });
    expect(
      await NotificationOwner.deliver({'type': 'action', 'action': 'reply'}),
      isFalse,
    );
    expect(calls, 1);
  });
}

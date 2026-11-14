import 'dart:async';

import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_flutter/bridge.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  WaddleBridge create({CommandInvoker? invoke, Stream<dynamic>? events}) =>
      WaddleBridge(
        commandInvoker: invoke ?? (method, arguments) async => null,
        events: events ?? const Stream<dynamic>.empty(),
      );

  test(
    'account generation clears conversation data and rejects stale events',
    () {
      final bridge = create();
      addTearDown(bridge.dispose);
      bridge.acceptEvent({
        'generation': 1,
        'topic': 'conversation',
        'data': {
          'jid': 'a@example.org',
          'messages': [
            {'body': 'private'},
          ],
        },
      });
      expect(bridge.conversation('a@example.org')['messages'], isNotEmpty);
      bridge.acceptEvent({
        'generation': 2,
        'topic': 'session',
        'data': {'state': 'signedOut'},
      });
      bridge.acceptEvent({
        'generation': 1,
        'topic': 'conversation',
        'data': {
          'jid': 'a@example.org',
          'messages': [
            {'body': 'late private'},
          ],
        },
      });
      expect(bridge.conversation('a@example.org'), isEmpty);
      expect(bridge.session['state'], 'signedOut');
    },
  );

  test('sign-out retains device appearance and removes account topics', () {
    final bridge = create();
    addTearDown(bridge.dispose);
    bridge.acceptEvent({
      'topic': 'preferences',
      'data': {'theme': 'dark'},
    });
    bridge.acceptEvent({
      'topic': 'profile',
      'data': {'nickname': 'Private'},
    });
    bridge.acceptEvent({
      'topic': 'session',
      'data': {'state': 'signedOut'},
    });
    expect(bridge.preferences['theme'], 'dark');
    expect(bridge.topic('profile'), isEmpty);
  });

  test('normalization preserves in-memory image bytes', () {
    final bytes = Uint8List.fromList([1, 2, 3]);
    final normalized =
        WaddleBridge.normalize(<Object?, Object?>{
              'avatar': bytes,
              'nested': <Object?, Object?>{
                'items': [1, 2],
              },
            })
            as Map<String, dynamic>;
    expect(normalized['avatar'], same(bytes));
    expect(normalized['nested'], isA<Map<String, dynamic>>());
  });

  test(
    'navigation is retained until acknowledged with the target JID',
    () async {
      final commands = <(String, Map<String, dynamic>)>[];
      final bridge = create(
        invoke: (method, args) async {
          commands.add((method, args));
          return null;
        },
      );
      addTearDown(bridge.dispose);
      bridge.acceptEvent({
        'topic': 'navigate',
        'data': {'jid': 'room@example.org', 'group': true},
      });
      expect(bridge.topic('navigate')['jid'], 'room@example.org');
      bridge.consumeNavigation();
      await Future<void>.delayed(Duration.zero);
      expect(bridge.topic('navigate'), isEmpty);
      expect(commands.single.$1, 'navigation.consume');
      expect(commands.single.$2, {'jid': 'room@example.org'});
    },
  );

  test('native errors remain actionable and can be dismissed', () async {
    final bridge = create(
      invoke: (method, args) async {
        throw PlatformException(
          code: 'operation_failed',
          message: 'Try again.',
        );
      },
    );
    addTearDown(bridge.dispose);
    await expectLater(
      bridge.invoke('room.create', {'name': 'New'}),
      throwsA(isA<PlatformException>()),
    );
    expect(bridge.error, 'Try again.');
    bridge.dismissError();
    expect(bridge.error, isNull);
  });

  test(
    'disposing the view detaches events and prevents further commands',
    () async {
      var cancelled = false;
      final stream = StreamController<dynamic>(
        onCancel: () => cancelled = true,
      );
      final bridge = create(events: stream.stream);
      bridge.dispose();
      await Future<void>.delayed(Duration.zero);
      expect(cancelled, isTrue);
      await expectLater(bridge.invoke('conversation.send'), throwsStateError);
      await stream.close();
    },
  );
}

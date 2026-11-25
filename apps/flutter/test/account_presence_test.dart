import 'dart:async';
import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_flutter/runtime/account_presence.dart';

void main() {
  testWidgets(
    'away and extended away use the time of actual input and an active call suppresses both',
    (tester) async {
      final initial = tester.binding.clock.now().toUtc();
      final sent = <Map<String, dynamic>>[];
      final state = AccountPresence(
        clock: () => tester.binding.clock.now(),
        changed: (_) {},
        request: (method, args) async {
          if (method == 'send_presence') sent.add(args);
          return {'type': 'automatic'};
        },
      );
      addTearDown(state.clear);
      await state.connected();
      expect(sent.single['show'], isNull);
      await tester.pump(const Duration(minutes: 8));
      state.interaction();
      await tester.pump(const Duration(minutes: 2));
      expect(sent, hasLength(1));
      await tester.pump(const Duration(minutes: 8));
      expect(sent.last['show'], 'away');
      expect(
        DateTime.parse(sent.last['idle_since']),
        initial.add(const Duration(minutes: 8)),
      );
      await tester.pump(const Duration(minutes: 20));
      expect(sent.last['show'], 'xa');
      state.call(true);
      await tester.pump();
      expect(sent.last, {'show': null, 'idle_since': null, 'status': null});
      await tester.pump(const Duration(hours: 1));
      expect(sent.last['show'], isNull);
      state.call(false);
      await tester.pump();
      await tester.pump(const Duration(minutes: 10));
      expect(sent.last['show'], 'away');
      state.clear();
    },
  );

  testWidgets(
    'manual picks persist across reconnect, a reset resumes automatic, and another device beats a stale fetch',
    (tester) async {
      final sent = <Map<String, dynamic>>[];
      final published = <Map<String, dynamic>>[];
      final snapshots = <Map<String, dynamic>>[];
      var stored = <String, dynamic>{'type': 'manual', 'status': 'dnd'};
      Completer<dynamic>? deferred;
      final state = AccountPresence(
        clock: () => tester.binding.clock.now(),
        changed: snapshots.add,
        request: (method, args) async {
          if (method == 'send_presence') {
            sent.add(args);
            return null;
          }
          if (method == 'publish_status_preference') {
            stored = Map<String, dynamic>.from(args['preference']);
            published.add(stored);
            return null;
          }
          return deferred?.future ?? stored;
        },
      );
      await state.connected();
      expect(sent.single['show'], 'dnd');
      await tester.pump(const Duration(hours: 1));
      expect(sent, hasLength(1));
      await state.select('chat');
      expect(published.single, {'type': 'manual', 'status': 'chat'});
      state.disconnect();
      expect(snapshots.last['show'], 'offline');
      await state.connected();
      expect(sent.last['show'], 'chat');
      deferred = Completer<dynamic>();
      final fetching = state.refresh();
      await state.adopt({'type': 'manual', 'status': 'away'});
      deferred.complete({'type': 'automatic'});
      await fetching;
      expect(snapshots.last['mode'], 'away');
      await state.select('automatic');
      expect(published.last, {'type': 'automatic'});
      expect(sent.last['show'], null);
      await tester.pump(const Duration(minutes: 9));
      expect(sent.last['show'], null);
      state.clear();
    },
  );
}

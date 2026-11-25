import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_flutter/bridge.dart';
import 'package:waddle_flutter/features/calendar_subscription.dart';
import 'package:waddle_flutter/runtime/surfaces_runtime.dart';

import 'platform_test_context.dart';

void main() {
  const sample =
      'https://calendar.example.test/api/calendar/community/example/events.ics';
  Future<void> menu(WidgetTester tester, String label) async {
    await tester.tap(find.byTooltip('Calendar subscription'));
    await tester.pumpAndSettle();
    await tester.tap(find.text(label));
    await tester.pumpAndSettle();
  }

  WaddleBridge bridge(CommandInvoker invoke) =>
      WaddleBridge(commandInvoker: invoke, events: const Stream.empty())
        ..acceptEvent({
          'topic': 'session',
          'generation': 1,
          'data': {'jid': 'alice@example.test'},
        });
  Future<void> mount(WidgetTester tester, WaddleBridge bridge) =>
      tester.pumpWidget(
        MaterialApp(
          home: Scaffold(body: CalendarSubscriptionMenu(bridge: bridge)),
        ),
      );
  testWidgets(
    'private URL stays out of presentation and is copied only on an explicit clipboard gesture',
    (tester) async {
      final calls = <String>[], copied = <String>[];
      final messenger =
          TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
      messenger.setMockMethodCallHandler(SystemChannels.platform, (call) async {
        if (call.method == 'Clipboard.setData') {
          copied.add(call.arguments['text'] as String);
        }
        return null;
      });
      addTearDown(
        () => messenger.setMockMethodCallHandler(SystemChannels.platform, null),
      );
      final b = bridge((method, args) async {
        calls.add(method);
        return {'status': 'available', 'url': sample};
      });
      addTearDown(b.dispose);
      await mount(tester, b);
      await menu(tester, 'Copy private subscription link');
      expect(find.text(sample), findsNothing);
      expect(calls, ['calendar.issueSubscription']);
      expect(copied, isEmpty);
      await tester.tap(find.text('Copy link'));
      await tester.pumpAndSettle();
      expect(copied, [sample]);
      expect(find.text('Private calendar link copied.'), findsOneWidget);
    },
  );
  testWidgets(
    'cancel keeps subscription alive and unsupported servers expose a usable static export',
    (tester) async {
      final calls = <String>[];
      final b = bridge((method, args) async {
        calls.add(method);
        return {'status': 'unsupported'};
      });
      addTearDown(b.dispose);
      await mount(tester, b);
      await menu(tester, 'Revoke subscription link');
      await tester.tap(find.text('Cancel'));
      await tester.pumpAndSettle();
      expect(calls, isEmpty);
      await menu(tester, 'Revoke subscription link');
      await tester.tap(find.text('Revoke'));
      await tester.pumpAndSettle();
      expect(calls, ['calendar.revokeSubscription']);
      expect(find.textContaining('export individual events'), findsOneWidget);
    },
  );
  testWidgets(
    'a late account response cannot put its bearer on the clipboard or open the copy dialog',
    (tester) async {
      final delayed = Completer<dynamic>();
      final b = bridge((_, _) => delayed.future);
      addTearDown(b.dispose);
      await mount(tester, b);
      await menu(tester, 'Copy private subscription link');
      b.acceptEvent({
        'topic': 'session',
        'generation': 2,
        'data': {'jid': 'bob@example.test'},
      });
      delayed.complete({'status': 'available', 'url': sample});
      await tester.pumpAndSettle();
      expect(find.text('Copy link'), findsNothing);
      expect(find.text(sample), findsNothing);
    },
  );
  testWidgets(
    'account replacement discards a private link awaiting clipboard consent',
    (tester) async {
      final b = bridge((_, _) async => {'status': 'available', 'url': sample});
      addTearDown(b.dispose);
      await mount(tester, b);
      await menu(tester, 'Copy private subscription link');
      expect(find.text('Copy link'), findsOneWidget);
      b.acceptEvent({
        'topic': 'session',
        'generation': 2,
        'data': {'jid': 'bob@example.test'},
      });
      await tester.pumpAndSettle();
      expect(find.text('Copy link'), findsNothing);
    },
  );
  test(
    'subscription commands only return the token to the requesting caller',
    () async {
      final ctx = TestRuntimeContext()
        ..handler = (method, args) async => args['revoke'] == true
            ? {'status': 'revoked'}
            : {'status': 'available', 'url': sample};
      final surface = SurfaceRuntime(ctx);
      addTearDown(surface.dispose);
      expect(
        (await surface.invoke('calendar.issueSubscription', {}))['url'],
        sample,
      );
      expect(
        (await surface.invoke('calendar.revokeSubscription', {}))['status'],
        'revoked',
      );
      expect(ctx.calls.map((c) => c['method']), [
        'calendar_subscription',
        'calendar_subscription',
      ]);
      expect(ctx.persistent, isEmpty);
      expect(ctx.emitted, isEmpty);
    },
  );
}

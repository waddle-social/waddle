import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_flutter/app.dart';
import 'package:waddle_flutter/bridge.dart';

void main() {
  testWidgets(
    'editing a homeserver fences its old providers and cancels stale requests',
    (tester) async {
      final requests = <String>[];
      final pending = Completer<void>();
      final selected = <String>[];
      const defaultOrigin = 'https://xmpp.waddle.social';
      late final WaddleBridge bridge;
      void projectServer(String origin, String name) {
        bridge.acceptEvent({
          'topic': 'session',
          'data': {'state': 'signedOut', 'serverOrigin': origin},
        });
        bridge.acceptEvent({
          'topic': 'login',
          'data': {
            'state': 'providers',
            'providers': [
              {'id': name, 'name': name},
            ],
          },
        });
      }

      bridge = WaddleBridge(
        events: const Stream.empty(),
        commandInvoker: (method, args) async {
          if (method == 'login.server') {
            final address = args['address'] as String;
            requests.add(address);
            if (address != defaultOrigin) {
              await pending.future;
            } else {
              projectServer(defaultOrigin, 'Colony');
            }
          } else if (method == 'login.select') {
            selected.add(args['id'] as String);
          }
          return null;
        },
      );
      addTearDown(bridge.dispose);
      projectServer(defaultOrigin, 'Colony');
      await tester.pumpWidget(MaterialApp(home: WaddleShell(bridge: bridge)));
      await tester.tap(find.text('Edit'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('Other server'));
      await tester.pumpAndSettle();
      expect(find.text('Sign in with Colony'), findsNothing);
      await tester.enterText(find.byType(TextField), 'http://example.org');
      await tester.testTextInput.receiveAction(TextInputAction.done);
      await tester.pump();
      expect(find.text('Enter an HTTPS server address.'), findsOneWidget);
      expect(requests, isEmpty);
      await tester.enterText(find.byType(TextField), 'one.example.org');
      await tester.pump(const Duration(milliseconds: 250));
      await tester.enterText(find.byType(TextField), 'two.example.org');
      await tester.pump(const Duration(milliseconds: 550));
      expect(requests, ['https://two.example.org']);
      await tester.ensureVisible(find.text('DEFAULT'));
      await tester.tap(find.text('DEFAULT'));
      await tester.pumpAndSettle();
      pending.complete();
      await tester.pumpAndSettle();
      expect(requests.last, defaultOrigin);
      await tester.tap(find.text('Hide'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('Sign in with Colony'));
      expect(selected, ['Colony']);
    },
  );

  testWidgets(
    'saved accounts and device approval remain reachable with a narrow viewport',
    (tester) async {
      tester.view.physicalSize = const Size(320, 640);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.reset);
      final calls = <(String, Map<String, dynamic>)>[];
      final bridge = WaddleBridge(
        events: const Stream.empty(),
        commandInvoker: (name, args) async {
          calls.add((name, args));
          return null;
        },
      );
      addTearDown(bridge.dispose);
      const account = {
        'jid': 'me@example.org',
        'origin': 'https://example.org',
      };
      bridge.acceptEvent({
        'topic': 'session',
        'data': {
          'state': 'signedOut',
          'serverOrigin': 'https://example.org',
          'canSwitchAccounts': true,
        },
      });
      bridge.acceptEvent({
        'topic': 'accounts',
        'data': {
          'saved': [account],
        },
      });
      bridge.acceptEvent({
        'topic': 'login',
        'data': {
          'state': 'providers',
          'providers': [
            {'id': 'work', 'name': 'Work'},
            {'id': 'community', 'name': 'Community'},
          ],
        },
      });
      await tester.pumpWidget(MaterialApp(home: WaddleShell(bridge: bridge)));
      expect(find.text('Sign in to Waddle'), findsOneWidget);
      expect(find.text('SIGN IN WITH'), findsOneWidget);
      await tester.ensureVisible(find.text(account['jid']!));
      await tester.tap(find.text(account['jid']!));
      expect(
        calls.where((call) => call.$1 == 'session.resume').single.$2,
        account,
      );
      bridge.acceptEvent({
        'topic': 'login',
        'data': {'state': 'verifying', 'code': 'TEST-1234'},
      });
      await tester.pump();
      expect(find.text(account['jid']!), findsNothing);
      expect(find.byType(SelectableText), findsOneWidget);
      await tester.ensureVisible(find.text('Cancel'));
      await tester.tap(find.text('Cancel'));
      expect(calls.last.$1, 'login.cancel');
      expect(tester.takeException(), isNull);
    },
  );
}

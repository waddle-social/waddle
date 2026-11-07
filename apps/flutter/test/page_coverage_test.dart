import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_flutter/bridge.dart';
import 'package:waddle_flutter/features/conversation.dart';

void main() {
  Future<(WaddleBridge, List<String>, GlobalKey<NavigatorState>)> mount(
    WidgetTester tester,
  ) async {
    final calls = <String>[];
    final navigator = GlobalKey<NavigatorState>();
    final bridge = WaddleBridge(
      events: const Stream<dynamic>.empty(),
      commandInvoker: (method, args) async {
        calls.add('$method:${args['visible']}');
        return null;
      },
    );
    bridge.acceptEvent({
      'topic': 'conversation',
      'data': {
        'jid': 'person@example.org',
        'messages': [],
        'loading': false,
        'reachedStart': true,
      },
    });
    tester.view.physicalSize = const Size(430, 1100);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    addTearDown(bridge.dispose);
    await tester.pumpWidget(
      MaterialApp(
        navigatorKey: navigator,
        navigatorObservers: [waddleRoutes],
        home: ConversationScreen(
          bridge: bridge,
          jid: 'person@example.org',
          title: 'Person',
          group: false,
        ),
      ),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    calls.clear();
    return (bridge, calls, navigator);
  }

  testWidgets('composer overlays do not close and reopen the conversation', (
    tester,
  ) async {
    final (_, calls, _) = await mount(tester);
    await tester.tap(find.byTooltip('More message options'));
    await tester.pumpAndSettle();
    expect(calls.where((call) => call.startsWith('conversation.')), isEmpty);

    await tester.tap(find.text('GIFs'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 600));
    expect(calls.where((call) => call.startsWith('conversation.')), isEmpty);

    await tester.binding.handlePopRoute();
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 600));
    expect(calls.where((call) => call.startsWith('conversation.')), isEmpty);
    await tester.pumpWidget(const SizedBox.shrink());
  });

  testWidgets('a page over an open menu still covers the conversation', (
    tester,
  ) async {
    final (_, calls, navigator) = await mount(tester);
    await tester.tap(find.byTooltip('More message options'));
    await tester.pumpAndSettle();
    navigator.currentState!.push<void>(
      MaterialPageRoute<void>(
        builder: (_) => const Scaffold(body: Text('Other page')),
      ),
    );
    await tester.pumpAndSettle();
    expect(calls, contains('conversation.visibility:false'));
    calls.clear();

    navigator.currentState!.pop();
    await tester.pumpAndSettle();
    expect(calls, contains('conversation.visibility:true'));
    expect(calls, contains('conversation.read:null'));
    await tester.pumpWidget(const SizedBox.shrink());
  });
}

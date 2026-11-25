import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_flutter/bridge.dart';
import 'package:waddle_flutter/features/compact_call.dart';
import 'package:waddle_flutter/features/surfaces.dart';

void main() {
  const call = {'state': 'active', 'sid': 'alpha', 'microphoneEnabled': false};
  final commands = <List<Object>>[];
  late WaddleBridge bridge;

  void setupCall({Widget Function(BuildContext, bool)? video}) {
    commands.clear();
    bridge =
        WaddleBridge(
          events: const Stream.empty(),
          commandInvoker: (method, args) async {
            commands.add([method, args]);
            return null;
          },
          callVideoBuilder: video,
        )..acceptEvent({
          'topic': 'call',
          'data': {...call, 'video': video != null},
        });
    addTearDown(bridge.dispose);
  }

  const press = [
    'call.talk',
    {'expectedSid': 'alpha', 'held': true},
  ];
  const release = [
    'call.talk',
    {'expectedSid': 'alpha', 'held': false},
  ];

  testWidgets('a held microphone is released as a child takes focus', (
    tester,
  ) async {
    final child = FocusNode();
    addTearDown(child.dispose);
    setupCall(
      video: (_, _) => Focus(
        focusNode: child,
        onKeyEvent: (_, _) => KeyEventResult.handled,
        child: const SizedBox.expand(),
      ),
    );
    await tester.pumpWidget(
      MaterialApp(home: ActiveCallScreen(bridge: bridge)),
    );
    await tester.pumpAndSettle();
    await tester.sendKeyDownEvent(LogicalKeyboardKey.space);
    expect(commands, [press]);
    child.requestFocus();
    await tester.pump();
    expect(child.hasPrimaryFocus, isTrue);
    expect(commands, [press, release]);
    await tester.sendKeyUpEvent(LogicalKeyboardKey.space);
    expect(commands, [press, release]);
  });

  testWidgets('entering compact mode releases the hidden call shortcuts', (
    tester,
  ) async {
    setupCall();
    await tester.pumpWidget(
      MaterialApp(
        builder: (_, child) => Overlay.wrap(
          child: CompactCall(bridge: bridge, child: child!),
        ),
        home: ActiveCallScreen(bridge: bridge),
      ),
    );
    await tester.pumpAndSettle();
    await tester.sendKeyDownEvent(LogicalKeyboardKey.space);
    expect(commands, [press]);
    bridge.acceptEvent({
      'topic': 'call',
      'data': {...call, 'compact': true},
    });
    await tester.pumpAndSettle();
    expect(find.byTooltip('Expand call'), findsOneWidget);
    expect(commands, [press, release]);
    await tester.sendKeyUpEvent(LogicalKeyboardKey.space);
    await tester.sendKeyEvent(LogicalKeyboardKey.keyM);
    expect(commands, [press, release]);
    await tester.sendKeyEvent(LogicalKeyboardKey.enter);
    expect(commands.last, [
      'call.compact',
      {'expectedSid': 'alpha', 'enabled': false},
    ]);
  });
}

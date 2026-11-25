import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_flutter/features/call_stage.dart';

void main() {
  testWidgets(
    'phone stage follows speakers and screens, pins a tile and hides only the self camera',
    (tester) async {
      const you = CallTile(
        id: 'me-camera',
        name: 'You',
        self: true,
        screen: false,
        speaking: false,
      );
      const alex = CallTile(
        id: 'alex-camera',
        name: 'Alex',
        self: false,
        screen: false,
        speaking: false,
      );
      const bea = CallTile(
        id: 'bea-camera',
        name: 'Bea',
        self: false,
        screen: false,
        speaking: true,
      );
      const slides = CallTile(
        id: 'me-screen',
        name: 'You',
        self: true,
        screen: true,
        speaking: false,
      );
      Future<void> stage(List<CallTile> tiles, String? active) =>
          tester.pumpWidget(
            MaterialApp(
              home: Scaffold(
                body: SizedBox(
                  width: 320,
                  height: 400,
                  child: CallStage(tiles: tiles, activeSpeaker: active),
                ),
              ),
            ),
          );
      double width(CallTile tile) =>
          tester.getSize(find.byKey(ValueKey(tile.id))).width;
      await stage([alex, bea, you], 'bea-camera');
      expect(width(bea), 320);
      expect(find.bySemanticsLabel(RegExp('Bea, speaking')), findsOneWidget);
      await tester.tap(find.byTooltip('Pin Alex'));
      await tester.pump();
      await stage([alex, bea, you, slides], 'bea-camera');
      expect(width(alex), 320);
      await tester.tap(find.byTooltip('Unpin Alex'));
      await tester.pump();
      expect(width(slides), 320);
      await tester.tap(find.byTooltip('Gallery view'));
      await tester.pump();
      expect(width(alex), lessThan(170));
      await tester.tap(find.byTooltip('Hide self-view'));
      await tester.pump();
      expect(find.byKey(const ValueKey('me-camera')), findsNothing);
      expect(find.byKey(const ValueKey('me-screen')), findsOneWidget);
      await tester.tap(find.byTooltip('Show self-view'));
      await tester.pump();
      expect(find.byKey(const ValueKey('me-camera')), findsOneWidget);
      expect(tester.takeException(), isNull);
    },
  );
}

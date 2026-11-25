import 'dart:async';
import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_flutter/features/story_camera.dart';
import 'package:waddle_flutter/features/story_composer.dart';
import 'package:waddle_flutter/runtime/story_recording.dart';

class _Camera implements StoryCamera {
  final clip = _Clip();
  int closes = 0;
  @override
  Widget get preview => const Text('Camera preview');
  @override
  Future<void> close() async {
    closes++;
  }

  @override
  Future<StoryRecording> record() async => clip;
  @override
  Future<StoryMedia> photo() async => throw UnimplementedError();
}

class _Clip implements StoryRecording {
  var cancelled = false, finished = false;
  @override
  Future<void> cancel() async {
    cancelled = true;
  }

  @override
  Future<StoryMedia> finish() async {
    finished = true;
    return StoryMedia(Uint8List.fromList([1, 2, 3]), 'story.mp4', 'video/mp4');
  }
}

Future<void> composer(
  WidgetTester tester,
  Future<StoryCamera> Function(bool) open,
  void Function(dynamic) result,
) async {
  await tester.pumpWidget(
    MaterialApp(
      home: Builder(
        builder: (context) => Scaffold(
          body: TextButton(
            onPressed: () async {
              result(
                await showDialog<dynamic>(
                  context: context,
                  builder: (_) => StoryComposer(open: open),
                ),
              );
            },
            child: const Text('New'),
          ),
        ),
      ),
    ),
  );
  await tester.tap(find.text('New'));
  await tester.pumpAndSettle();
}

void main() {
  testWidgets(
    'a recorded video releases the camera and publishes only after confirmation',
    (tester) async {
      final camera = _Camera();
      dynamic sent;
      await composer(tester, (_) async => camera, (v) => sent = v);
      await tester.enterText(find.byType(TextField), 'Meeting the penguins');
      await tester.tap(find.text('Record video'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('Record up to 10 seconds'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('Finish recording'));
      await tester.pumpAndSettle();
      expect(camera.closes, 1);
      expect(camera.clip.finished, true);
      expect(sent, null);
      await tester.tap(find.text('Publish story'));
      await tester.pumpAndSettle();
      expect(sent['body'], 'Meeting the penguins');
      expect(sent['capture']['media_type'], 'video/mp4');
    },
  );
  testWidgets(
    'cancelling during recording releases camera, mic and unfinished output',
    (tester) async {
      final camera = _Camera();
      await composer(tester, (_) async => camera, (_) {});
      await tester.tap(find.text('Record video'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('Record up to 10 seconds'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('Cancel').last);
      await tester.pumpAndSettle();
      expect(camera.closes, 1);
      expect(camera.clip.cancelled, true);
      expect(find.text('Publish story'), findsNothing);
    },
  );
  testWidgets(
    'a denied camera falls back to the picker and keeps the caption',
    (tester) async {
      dynamic sent;
      await composer(
        tester,
        (_) async => throw StateError('denied'),
        (v) => sent = v,
      );
      await tester.enterText(find.byType(TextField), 'Hello');
      await tester.tap(find.text('Take photo'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('Choose existing media').last);
      await tester.pumpAndSettle();
      expect(sent, {'body': 'Hello'});
    },
  );
  testWidgets(
    'the camera is released if permission resolves after cancellation',
    (tester) async {
      final permission = Completer<StoryCamera>(), camera = _Camera();
      await composer(tester, (_) => permission.future, (_) {});
      await tester.tap(find.text('Take photo'));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 350));
      await tester.tap(find.text('Cancel').last);
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 350));
      permission.complete(camera);
      await tester.pumpAndSettle();
      expect(camera.closes, 1);
    },
  );
}

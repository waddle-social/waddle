import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  testWidgets(
    'Android scopes its picture-in-picture activity to an active call',
    (tester) async {
      const window = MethodChannel('social.waddle.flutter/call_window');
      final changes = <bool>[];
      window.setMethodCallHandler((event) async {
        if (event.method == 'changed') changes.add(event.arguments as bool);
      });
      try {
        await tester.pumpWidget(
          const MaterialApp(
            home: Scaffold(body: Center(child: Text('Active call'))),
          ),
        );
        await window.invokeMethod<void>('setActive', false);
        expect(await window.invokeMethod<bool>('setCompact', true), false);
        try {
          await window.invokeMethod<void>('setActive', true);
        } on PlatformException catch (error) {
          expect(error.code, 'pip_unavailable');
          expect(const bool.fromEnvironment('WADDLE_EXPECT_PIP'), false);
          expect(await window.invokeMethod<bool>('state'), false);
          return;
        }
        final entered = await window.invokeMethod<bool>('setCompact', true);
        if (const bool.fromEnvironment('WADDLE_EXPECT_PIP')) {
          expect(
            entered,
            true,
            reason:
                'The system test image must implement Android picture-in-picture.',
          );
        }
        expect(await window.invokeMethod<bool>('state'), entered);
        if (entered == true) {
          expect(changes, contains(true));
          expect(await window.invokeMethod<bool>('setCompact', false), false);
          expect(await window.invokeMethod<bool>('state'), false);
          expect(changes, containsAllInOrder([true, false]));
          // Android may decline immediate re-entry while the task is expanding.
          final expansion = Stopwatch()..start();
          while (await window.invokeMethod<bool>('setCompact', true) != true) {
            expect(expansion.elapsed, lessThan(const Duration(seconds: 5)));
            await Future<void>.delayed(const Duration(milliseconds: 100));
          }
          await window.invokeMethod<void>('setActive', false);
          final deadline = Stopwatch()..start();
          while (await window.invokeMethod<bool>('state') == true &&
              deadline.elapsed < const Duration(seconds: 5)) {
            await Future<void>.delayed(const Duration(milliseconds: 50));
          }
          expect(await window.invokeMethod<bool>('state'), false);
          expect(await window.invokeMethod<bool>('setCompact', true), false);
        }
      } finally {
        await window.invokeMethod<void>('setActive', false);
        window.setMethodCallHandler(null);
      }
    },
    skip: !Platform.isAndroid,
  );
}

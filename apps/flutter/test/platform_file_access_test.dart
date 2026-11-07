import 'dart:async';
import 'dart:io';

import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_flutter/runtime/platform_file_access_io.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  test('local URI reads enforce a size limit and validate ownership', () async {
    final directory = await Directory.systemTemp.createTemp('waddle-file-test');
    addTearDown(() => directory.delete(recursive: true));
    final file = File('${directory.path}/image.bin');
    await file.writeAsBytes([1, 2, 3, 4]);
    expect(await readLocalFileUri(file.uri.toString(), maxBytes: 4), [
      1,
      2,
      3,
      4,
    ]);
    await expectLater(
      readLocalFileUri(file.uri.toString(), maxBytes: 3),
      throwsStateError,
    );
    await expectLater(
      readLocalFileUri(
        file.uri.toString(),
        maxBytes: 4,
        validate: () => throw StateError('account changed'),
      ),
      throwsStateError,
    );
  });

  test('new private cache waits for previous account cleanup', () async {
    final directory = await Directory.systemTemp.createTemp('waddle-open-test');
    final initialPath = Completer<String>();
    var calls = 0;
    const channel = MethodChannel('plugins.flutter.io/path_provider');
    TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .setMockMethodCallHandler(channel, (call) {
          expect(call.method, 'getTemporaryDirectory');
          return ++calls == 1
              ? initialPath.future
              : Future.value(directory.path);
        });
    addTearDown(() async {
      await clearPrivateFiles();
      TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
          .setMockMethodCallHandler(channel, null);
      await directory.delete(recursive: true);
    });

    final oldPreparation = preparePrivateFiles();
    await Future<void>.delayed(Duration.zero);
    final oldCleanup = clearPrivateFiles();
    final newPreparation = preparePrivateFiles();
    initialPath.complete(directory.path);
    await oldPreparation;
    await oldCleanup;
    await newPreparation;
    expect(await Directory('${directory.path}/waddle-open').exists(), isTrue);
    expect(calls, 2);
  });
}

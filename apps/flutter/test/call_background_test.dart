import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_flutter/runtime/call_camera.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  test(
    'camera settings decode local images to bounded straight RGBA',
    () async {
      final original = await rootBundle.load('assets/backgrounds/mountain.png');
      for (final name in ['mountain', 'office', 'abstract', 'custom']) {
        final effect = await CameraBackground.from(
          name,
          name == 'custom' ? Uint8List.sublistView(original) : null,
        );
        final image = effect.image;
        expect(effect.name, name);
        expect(image['width'], inInclusiveRange(1, 1024));
        expect(image['height'], inInclusiveRange(1, 1024));
        expect(
          (image['rgba'] as Uint8List).length,
          image['width'] * image['height'] * 4,
        );
      }
    },
  );
  test(
    'an absent custom image and unknown effect fail before attaching',
    () async {
      await expectLater(
        CameraBackground.from('custom', null),
        throwsArgumentError,
      );
      await expectLater(
        CameraBackground.from('unknown', null),
        throwsArgumentError,
      );
      for (final mode in ['blur', 'off']) {
        expect((await CameraBackground.from(mode, null)).image, isEmpty);
      }
    },
  );
}

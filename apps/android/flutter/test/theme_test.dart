import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_android/main.dart' as app;
import 'package:waddle_android/design.dart';

double contrast(Color a, Color b) {
  final one = a.computeLuminance();
  final two = b.computeLuminance();
  return ((one > two ? one : two) + .05) / ((one < two ? one : two) + .05);
}

void main() {
  for (final brightness in Brightness.values) {
    test(
      'clubhouse colors meet normal-text contrast in ${brightness.name}',
      () {
        final c = waddleScheme(brightness);
        final pairs = [
          (c.onSurface, c.surface),
          (c.onSurfaceVariant, c.surface),
          (c.onSurfaceVariant, c.surfaceContainerLowest),
          (c.onSurfaceVariant, c.surfaceContainerHighest),
          (c.onPrimary, c.primary),
          (c.onPrimaryContainer, c.primaryContainer),
          (c.onSecondary, c.secondary),
          (c.onSecondaryContainer, c.secondaryContainer),
          (c.onTertiary, c.tertiary),
          (c.onTertiaryContainer, c.tertiaryContainer),
        ];
        for (final (foreground, background) in pairs) {
          expect(
            contrast(foreground, background),
            greaterThanOrEqualTo(4.5),
            reason:
                '$foreground on $background must support normal-size labels',
          );
        }
      },
    );
  }

  test(
    'Android signed ARGB colors preserve their channels and scheme brightness',
    () {
      final fallback = ColorScheme.fromSeed(
        seedColor: Colors.teal,
        brightness: Brightness.dark,
      );
      final scheme = app.wallpaperScheme(fallback, {
        'primary': -16711936,
        'surfaceContainer': -16776961,
        'onPrimary': -1,
        'secondaryFixed': -65536,
      });
      expect(scheme.primary, const Color(0xff00ff00));
      expect(scheme.surfaceContainer, const Color(0xff0000ff));
      expect(scheme.onPrimary, const Color(0xffffffff));
      expect(scheme.secondaryFixed, const Color(0xffff0000));
      expect(scheme.brightness, Brightness.dark);
      expect(scheme.error, fallback.error);
    },
  );
}

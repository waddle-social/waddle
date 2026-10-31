import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_android/main.dart' as app;

void main() {
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

import 'dart:async';

import 'package:flutter/material.dart';

import 'app.dart';
import 'bridge.dart';

void main() {
  WidgetsFlutterBinding.ensureInitialized();
  runApp(WaddleApp(bridge: WaddleBridge()));
}

class WaddleApp extends StatefulWidget {
  const WaddleApp({super.key, required this.bridge});
  final WaddleBridge bridge;

  @override
  State<WaddleApp> createState() => _WaddleAppState();
}

class _WaddleAppState extends State<WaddleApp> {
  @override
  void initState() {
    super.initState();
    unawaited(widget.bridge.invoke('ready').catchError((Object _) => null));
  }

  @override
  void dispose() {
    widget.bridge.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => AnimatedBuilder(
    animation: widget.bridge,
    builder: (context, _) {
      final prefs = widget.bridge.preferences;
      final dynamicColors = prefs['wallpaperColors'] == true;
      ThemeData theme(Brightness brightness) {
        final seedScheme = ColorScheme.fromSeed(
          seedColor: const Color(0xFF006B60),
          brightness: brightness,
        );
        final palette = widget.bridge.topic(
          'palette',
        )[brightness == Brightness.light ? 'light' : 'dark'];
        final scheme = dynamicColors && palette is Map
            ? wallpaperScheme(seedScheme, Map<String, dynamic>.from(palette))
            : seedScheme;
        return ThemeData(
          useMaterial3: true,
          colorScheme: scheme,
          scaffoldBackgroundColor: scheme.surface,
          appBarTheme: AppBarTheme(
            centerTitle: false,
            backgroundColor: scheme.surface,
            foregroundColor: scheme.onSurface,
          ),
          inputDecorationTheme: InputDecorationTheme(
            filled: true,
            fillColor: scheme.surfaceContainerHighest,
            border: const OutlineInputBorder(
              borderRadius: BorderRadius.all(Radius.circular(20)),
              borderSide: BorderSide.none,
            ),
          ),
          pageTransitionsTheme: const PageTransitionsTheme(
            builders: {
              TargetPlatform.android: PredictiveBackPageTransitionsBuilder(),
            },
          ),
        );
      }

      return MaterialApp(
        title: 'Waddle',
        restorationScopeId: 'waddle',
        debugShowCheckedModeBanner: false,
        theme: theme(Brightness.light),
        darkTheme: theme(Brightness.dark),
        themeMode: switch (prefs['theme']) {
          'light' => ThemeMode.light,
          'dark' => ThemeMode.dark,
          _ => ThemeMode.system,
        },
        builder: (context, child) => Column(
          children: [
            if (widget.bridge.error != null)
              Material(
                color: Theme.of(context).colorScheme.errorContainer,
                child: SafeArea(
                  bottom: false,
                  child: Padding(
                    padding: const EdgeInsets.fromLTRB(16, 8, 8, 8),
                    child: Row(
                      children: [
                        Expanded(
                          child: Text(
                            widget.bridge.error!,
                            style: TextStyle(
                              color: Theme.of(
                                context,
                              ).colorScheme.onErrorContainer,
                            ),
                          ),
                        ),
                        IconButton(
                          onPressed: widget.bridge.dismissError,
                          tooltip: 'Dismiss error',
                          icon: const Icon(Icons.close),
                        ),
                      ],
                    ),
                  ),
                ),
              ),
            Expanded(child: child ?? const SizedBox.shrink()),
          ],
        ),
        home: WaddleShell(bridge: widget.bridge),
      );
    },
  );
}

/// Android supplies the wallpaper roles through its existing Material 3 API.
/// This keeps dynamic color inside the host's supported Android toolchain.
ColorScheme wallpaperScheme(ColorScheme fallback, Map<String, dynamic> roles) {
  Color? color(String role) {
    final value = roles[role];
    return value is num ? Color(value.toInt() & 0xffffffff) : null;
  }

  return fallback.copyWith(
    primary: color('primary'),
    onPrimary: color('onPrimary'),
    primaryContainer: color('primaryContainer'),
    onPrimaryContainer: color('onPrimaryContainer'),
    primaryFixed: color('primaryFixed'),
    primaryFixedDim: color('primaryFixedDim'),
    onPrimaryFixed: color('onPrimaryFixed'),
    onPrimaryFixedVariant: color('onPrimaryFixedVariant'),
    inversePrimary: color('inversePrimary'),
    secondary: color('secondary'),
    onSecondary: color('onSecondary'),
    secondaryContainer: color('secondaryContainer'),
    onSecondaryContainer: color('onSecondaryContainer'),
    secondaryFixed: color('secondaryFixed'),
    secondaryFixedDim: color('secondaryFixedDim'),
    onSecondaryFixed: color('onSecondaryFixed'),
    onSecondaryFixedVariant: color('onSecondaryFixedVariant'),
    tertiary: color('tertiary'),
    onTertiary: color('onTertiary'),
    tertiaryContainer: color('tertiaryContainer'),
    onTertiaryContainer: color('onTertiaryContainer'),
    tertiaryFixed: color('tertiaryFixed'),
    tertiaryFixedDim: color('tertiaryFixedDim'),
    onTertiaryFixed: color('onTertiaryFixed'),
    onTertiaryFixedVariant: color('onTertiaryFixedVariant'),
    error: color('error'),
    onError: color('onError'),
    errorContainer: color('errorContainer'),
    onErrorContainer: color('onErrorContainer'),
    surface: color('surface'),
    onSurface: color('onSurface'),
    onSurfaceVariant: color('onSurfaceVariant'),
    surfaceDim: color('surfaceDim'),
    surfaceBright: color('surfaceBright'),
    surfaceContainerLowest: color('surfaceContainerLowest'),
    surfaceContainerLow: color('surfaceContainerLow'),
    surfaceContainer: color('surfaceContainer'),
    surfaceContainerHigh: color('surfaceContainerHigh'),
    surfaceContainerHighest: color('surfaceContainerHighest'),
    surfaceTint: color('surfaceTint'),
    inverseSurface: color('inverseSurface'),
    onInverseSurface: color('onInverseSurface'),
    outline: color('outline'),
    outlineVariant: color('outlineVariant'),
    scrim: color('scrim'),
    shadow: color('shadow'),
  );
}

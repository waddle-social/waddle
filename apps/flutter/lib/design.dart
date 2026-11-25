import 'dart:typed_data';

import 'package:flutter/material.dart';

/// Polar Huddle's Night and Daylight roles, as resolved by Waddle's web design preset.
ColorScheme waddleScheme(Brightness brightness) {
  Color role(int night, int day) =>
      Color(brightness == Brightness.dark ? night : day);
  return ColorScheme.fromSeed(
    seedColor: role(0xFFFFC139, 0xFF0F4376),
    brightness: brightness,
  ).copyWith(
    primary: role(0xFFFFC139, 0xFF0F4376),
    onPrimary: role(0xFF060F19, 0xFFFFFFFF),
    primaryContainer: role(0xFF2B1C00, 0xFFFFEED0),
    onPrimaryContainer: role(0xFFFFD27D, 0xFF6B4C00),
    secondary: role(0xFF78B5F9, 0xFF205F9F),
    onSecondary: role(0xFF060F19, 0xFFFFFFFF),
    secondaryContainer: role(0xFF041A32, 0xFFF1F8FF),
    onSecondaryContainer: role(0xFF9ECCFF, 0xFF205F9F),
    tertiary: role(0xFF4ED7DF, 0xFF007176),
    onTertiary: role(0xFF060F19, 0xFFFFFFFF),
    tertiaryContainer: role(0xFF002021, 0xFFEBFAFB),
    onTertiaryContainer: role(0xFF85E5EA, 0xFF005256),
    error: const Color(0xFFD33944),
    onError: const Color(0xFFFFFFFF),
    errorContainer: role(0xFF360307, 0xFFFFF4F3),
    onErrorContainer: role(0xFFFFAFAC, 0xFFAA1D2C),
    surface: role(0xFF060F19, 0xFFF8FAFD),
    onSurface: role(0xFFEDF0F4, 0xFF060F19),
    onSurfaceVariant: role(0xFFC1C8D0, 0xFF38424E),
    surfaceDim: role(0xFF020912, 0xFFEDF0F4),
    surfaceBright: role(0xFF232E39, 0xFFFFFFFF),
    surfaceContainerLowest: role(0xFF111A25, 0xFFFFFFFF),
    surfaceContainerLow: role(0xFF0A131E, 0xFFFFFFFF),
    surfaceContainer: role(0xFF111A25, 0xFFFFFFFF),
    surfaceContainerHigh: role(0xFF1A242F, 0xFFEDF0F4),
    surfaceContainerHighest: role(0xFF232E39, 0xFFFFFFFF),
    outline: role(0xFF9AA2AD, 0xFF737D88),
    outlineVariant: role(0xFF28333F, 0xFFDBE0E6),
    scrim: role(0xB8020912, 0x80060F19),
  );
}

ThemeData waddleTheme(ColorScheme scheme) {
  final base = ThemeData(
    useMaterial3: true,
    colorScheme: scheme,
    fontFamily: 'Atkinson Hyperlegible Next',
  );
  TextStyle display(
    double size,
    double lineHeight,
    FontWeight weight,
    double tracking,
  ) => TextStyle(
    fontFamily: 'Bricolage Grotesque',
    fontSize: size,
    height: lineHeight / size,
    fontWeight: weight,
    letterSpacing: tracking,
    color: scheme.onSurface,
  );
  TextStyle body(double size, double lineHeight, FontWeight weight) =>
      TextStyle(
        fontFamily: 'Atkinson Hyperlegible Next',
        fontSize: size,
        height: lineHeight / size,
        fontWeight: weight,
        letterSpacing: 0,
        color: scheme.onSurface,
      );
  final text = base.textTheme.copyWith(
    displayLarge: display(60, 60, FontWeight.w800, -1.8),
    displayMedium: display(40, 44, FontWeight.w800, -1),
    displaySmall: display(30, 36, FontWeight.w800, -.6),
    headlineLarge: display(30, 36, FontWeight.w800, -.6),
    headlineMedium: display(24, 30, FontWeight.w700, -.36),
    headlineSmall: display(20, 26, FontWeight.w700, -.2),
    titleLarge: display(20, 26, FontWeight.w700, -.2),
    titleMedium: display(18, 24, FontWeight.w700, -.18),
    titleSmall: body(15, 20, FontWeight.w700),
    bodyLarge: body(16, 24, FontWeight.w400),
    bodyMedium: body(15, 22, FontWeight.w400),
    bodySmall: body(
      13,
      18,
      FontWeight.w400,
    ).copyWith(color: scheme.onSurfaceVariant),
    labelLarge: body(14, 20, FontWeight.w600),
    labelMedium: body(13, 18, FontWeight.w600),
    labelSmall: body(
      12,
      16,
      FontWeight.w600,
    ).copyWith(color: scheme.onSurfaceVariant),
  );
  final shape = RoundedRectangleBorder(borderRadius: BorderRadius.circular(12));
  return base.copyWith(
    textTheme: text,
    scaffoldBackgroundColor: scheme.surface,
    splashFactory: InkSparkle.splashFactory,
    appBarTheme: AppBarTheme(
      centerTitle: false,
      toolbarHeight: 56,
      titleSpacing: 16,
      elevation: 0,
      scrolledUnderElevation: 0,
      backgroundColor: scheme.surface,
      foregroundColor: scheme.onSurface,
      titleTextStyle: text.titleLarge,
    ),
    cardTheme: CardThemeData(
      color: scheme.surfaceContainerLowest,
      elevation: 0,
      margin: EdgeInsets.zero,
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(20)),
    ),
    inputDecorationTheme: InputDecorationTheme(
      filled: true,
      fillColor: scheme.surfaceContainerLow,
      hintStyle: text.bodyMedium?.copyWith(color: scheme.onSurfaceVariant),
      contentPadding: const EdgeInsets.symmetric(horizontal: 14, vertical: 12),
      border: OutlineInputBorder(
        borderRadius: BorderRadius.circular(12),
        borderSide: BorderSide(color: scheme.outlineVariant),
      ),
      enabledBorder: OutlineInputBorder(
        borderRadius: BorderRadius.circular(12),
        borderSide: BorderSide(color: scheme.outlineVariant),
      ),
      focusedBorder: OutlineInputBorder(
        borderRadius: BorderRadius.circular(12),
        borderSide: BorderSide(color: scheme.primary, width: 2),
      ),
    ),
    searchBarTheme: SearchBarThemeData(
      elevation: const WidgetStatePropertyAll(0),
      backgroundColor: WidgetStatePropertyAll(scheme.surfaceContainerLowest),
      side: WidgetStatePropertyAll(BorderSide(color: scheme.outlineVariant)),
      shape: WidgetStatePropertyAll(shape),
      padding: const WidgetStatePropertyAll(
        EdgeInsets.symmetric(horizontal: 18),
      ),
      textStyle: WidgetStatePropertyAll(text.bodyMedium),
      hintStyle: WidgetStatePropertyAll(
        text.bodyMedium?.copyWith(color: scheme.onSurfaceVariant),
      ),
    ),
    filledButtonTheme: FilledButtonThemeData(
      style:
          FilledButton.styleFrom(
            minimumSize: const Size(44, 44),
            padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 10),
            textStyle: text.labelLarge,
            shape: shape,
          ).copyWith(
            backgroundBuilder: (context, states, child) =>
                states.contains(WidgetState.disabled)
                ? child ?? const SizedBox.shrink()
                : DecoratedBox(
                    decoration: const BoxDecoration(
                      gradient: LinearGradient(
                        begin: Alignment.topCenter,
                        end: Alignment.bottomCenter,
                        colors: [Color(0x33FFFFFF), Color(0x00FFFFFF)],
                      ),
                    ),
                    child: child,
                  ),
          ),
    ),
    outlinedButtonTheme: OutlinedButtonThemeData(
      style: OutlinedButton.styleFrom(
        minimumSize: const Size(44, 44),
        padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 10),
        textStyle: text.labelLarge,
        shape: shape,
      ),
    ),
    textButtonTheme: TextButtonThemeData(
      style: TextButton.styleFrom(
        minimumSize: const Size(48, 48),
        textStyle: text.labelLarge,
        shape: shape,
      ),
    ),
    iconButtonTheme: const IconButtonThemeData(
      style: ButtonStyle(
        minimumSize: WidgetStatePropertyAll(Size(48, 48)),
        iconSize: WidgetStatePropertyAll(22),
      ),
    ),
    floatingActionButtonTheme: FloatingActionButtonThemeData(
      elevation: 0,
      focusElevation: 0,
      hoverElevation: 1,
      highlightElevation: 0,
      backgroundColor: scheme.primary,
      foregroundColor: scheme.onPrimary,
      extendedTextStyle: text.labelLarge,
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(20)),
    ),
    navigationBarTheme: NavigationBarThemeData(
      height: 64,
      elevation: 0,
      backgroundColor: scheme.surfaceContainerLowest,
      indicatorColor: scheme.primaryContainer,
      labelTextStyle: WidgetStateProperty.resolveWith(
        (states) => text.labelSmall?.copyWith(
          color: states.contains(WidgetState.selected)
              ? scheme.onSurface
              : scheme.onSurfaceVariant,
        ),
      ),
    ),
    navigationRailTheme: NavigationRailThemeData(
      backgroundColor: scheme.surface,
      indicatorColor: scheme.primaryContainer,
      selectedLabelTextStyle: text.labelSmall?.copyWith(
        color: scheme.onSurface,
      ),
      unselectedLabelTextStyle: text.labelSmall,
    ),
    listTileTheme: ListTileThemeData(
      contentPadding: const EdgeInsets.symmetric(horizontal: 16, vertical: 2),
      iconColor: scheme.onSurfaceVariant,
      titleTextStyle: text.titleMedium,
      subtitleTextStyle: text.bodySmall,
    ),
    dividerTheme: DividerThemeData(
      color: scheme.outlineVariant,
      thickness: 1,
      space: 1,
    ),
    dialogTheme: DialogThemeData(
      backgroundColor: scheme.surfaceContainerLowest,
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(28)),
    ),
    bottomSheetTheme: BottomSheetThemeData(
      backgroundColor: scheme.surfaceContainerLowest,
      modalBackgroundColor: scheme.surfaceContainerLowest,
      showDragHandle: true,
      shape: const RoundedRectangleBorder(
        borderRadius: BorderRadius.vertical(top: Radius.circular(30)),
      ),
    ),
    snackBarTheme: SnackBarThemeData(
      behavior: SnackBarBehavior.floating,
      backgroundColor: scheme.inverseSurface,
      contentTextStyle: text.bodyMedium?.copyWith(
        color: scheme.onInverseSurface,
      ),
      shape: shape,
    ),
    pageTransitionsTheme: const PageTransitionsTheme(
      builders: {
        TargetPlatform.android: PredictiveBackPageTransitionsBuilder(),
      },
    ),
  );
}

class WaddleMark extends StatelessWidget {
  const WaddleMark({super.key, this.size = 36});
  final double size;
  @override
  Widget build(BuildContext context) => Image.asset(
    'assets/brand/waddle-mark.png',
    width: size,
    height: size,
    semanticLabel: 'Waddle',
  );
}

class WaddleBrand extends StatelessWidget {
  const WaddleBrand({super.key, this.compact = false});
  final bool compact;
  @override
  Widget build(BuildContext context) => Row(
    mainAxisSize: MainAxisSize.min,
    children: [
      const WaddleMark(),
      if (!compact) ...[
        const SizedBox(width: 10),
        Text(
          'waddle',
          style: TextStyle(
            fontFamily: 'Bricolage Grotesque',
            fontWeight: FontWeight.w600,
            fontSize: 28,
            height: 1,
            letterSpacing: -.6,
            color: Theme.of(context).colorScheme.onSurface,
          ),
        ),
      ],
    ],
  );
}

class WaddleAvatar extends StatelessWidget {
  const WaddleAvatar({
    super.key,
    required this.name,
    this.bytes,
    this.size = 44,
    this.online = false,
  });
  final String name;
  final Uint8List? bytes;
  final double size;
  final bool online;
  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    const tints = [
      Color(0xFFFFE0A9),
      Color(0xFFC4DFFF),
      Color(0xFFDBE0E6),
      Color(0xFFB4EEF1),
      Color(0xFFFFCFCC),
      Color(0xFFBCEAC7),
    ];
    final hash = name.runes.fold(
      5381,
      (value, rune) => (value * 33 + rune).toUnsigned(32),
    );
    final background = tints[hash % tints.length];
    const foreground = Color(0xFF060F19);
    final initials = name
        .trim()
        .split(RegExp(r'\s+'))
        .where((part) => part.isNotEmpty)
        .take(2)
        .map((part) => part.characters.first)
        .join()
        .toUpperCase();
    return Semantics(
      label: '$name${online ? ', online' : ''}',
      child: SizedBox.square(
        dimension: size,
        child: Stack(
          clipBehavior: Clip.none,
          children: [
            ClipOval(
              child: ColoredBox(
                color: background,
                child: SizedBox.expand(
                  child: bytes == null
                      ? Center(
                          child: Text(
                            initials.isEmpty ? '?' : initials,
                            style: TextStyle(
                              fontFamily: 'Atkinson Hyperlegible Next',
                              fontSize: size * .36,
                              fontWeight: FontWeight.w800,
                              color: foreground,
                            ),
                          ),
                        )
                      : Image.memory(
                          bytes!,
                          fit: BoxFit.cover,
                          errorBuilder: (_, _, _) =>
                              Icon(Icons.person_outline, color: foreground),
                        ),
                ),
              ),
            ),
            if (online)
              Positioned(
                right: -1,
                bottom: -1,
                child: DecoratedBox(
                  decoration: BoxDecoration(
                    color: scheme.brightness == Brightness.dark
                        ? const Color(0xFF67C986)
                        : const Color(0xFF0A924B),
                    shape: BoxShape.circle,
                    border: Border.all(color: scheme.surface, width: 2.5),
                  ),
                  child: const SizedBox.square(dimension: 13),
                ),
              ),
          ],
        ),
      ),
    );
  }
}

class WaddleSectionHeading extends StatelessWidget {
  const WaddleSectionHeading({
    super.key,
    required this.title,
    this.eyebrow,
    this.trailing,
  });
  final String title;
  final String? eyebrow;
  final Widget? trailing;
  @override
  Widget build(BuildContext context) => Row(
    crossAxisAlignment: CrossAxisAlignment.center,
    children: [
      Expanded(
        child: Semantics(
          header: true,
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              if (eyebrow != null) ...[
                Text(
                  eyebrow!.toUpperCase(),
                  style: Theme.of(
                    context,
                  ).textTheme.labelSmall?.copyWith(letterSpacing: 1.6),
                ),
                const SizedBox(height: 6),
              ],
              Text(title, style: Theme.of(context).textTheme.titleLarge),
            ],
          ),
        ),
      ),
      if (trailing != null) ...[const SizedBox(width: 12), trailing!],
    ],
  );
}

class WaddlePanel extends StatelessWidget {
  const WaddlePanel({
    super.key,
    required this.child,
    this.padding = const EdgeInsets.all(20),
    this.color,
    this.onTap,
  });
  final Widget child;
  final EdgeInsetsGeometry padding;
  final Color? color;
  final VoidCallback? onTap;
  @override
  Widget build(BuildContext context) => Material(
    color: color ?? Theme.of(context).colorScheme.surfaceContainerLowest,
    shape: RoundedRectangleBorder(
      borderRadius: BorderRadius.circular(20),
      side: BorderSide(color: Theme.of(context).colorScheme.outlineVariant),
    ),
    clipBehavior: Clip.antiAlias,
    child: InkWell(
      onTap: onTap,
      child: Padding(padding: padding, child: child),
    ),
  );
}

/// A quiet conversation-shaped flourish, kept out of the semantic tree.
class WaddleMotif extends StatelessWidget {
  const WaddleMotif({super.key, this.color});
  final Color? color;
  @override
  Widget build(BuildContext context) => ExcludeSemantics(
    child: IgnorePointer(
      child: CustomPaint(
        painter: _MotifPainter(
          color ?? Theme.of(context).colorScheme.primary.withValues(alpha: .12),
        ),
      ),
    ),
  );
}

class _MotifPainter extends CustomPainter {
  const _MotifPainter(this.color);
  final Color color;
  @override
  void paint(Canvas canvas, Size size) {
    canvas.save();
    canvas.translate(size.width * .5, size.height * .5);
    canvas.rotate(-.25);
    final paint = Paint()
      ..color = color
      ..style = PaintingStyle.stroke
      ..strokeWidth = 2;
    for (var i = 0; i < 3; i++) {
      final width = size.width * (.65 + i * .18);
      final height = size.height * (.42 + i * .18);
      final rect = Rect.fromCenter(
        center: Offset.zero,
        width: width,
        height: height,
      );
      canvas.drawRRect(
        RRect.fromRectAndRadius(rect, Radius.circular(height * .48)),
        paint,
      );
    }
    canvas.restore();
    final sparkle = Path()
      ..moveTo(size.width * .72, size.height * .08)
      ..quadraticBezierTo(
        size.width * .75,
        size.height * .22,
        size.width * .88,
        size.height * .24,
      )
      ..quadraticBezierTo(
        size.width * .75,
        size.height * .27,
        size.width * .72,
        size.height * .4,
      )
      ..quadraticBezierTo(
        size.width * .68,
        size.height * .27,
        size.width * .56,
        size.height * .24,
      )
      ..quadraticBezierTo(
        size.width * .68,
        size.height * .22,
        size.width * .72,
        size.height * .08,
      );
    canvas.drawPath(
      sparkle,
      Paint()
        ..color = color
        ..style = PaintingStyle.fill,
    );
  }

  @override
  bool shouldRepaint(_MotifPainter oldDelegate) => color != oldDelegate.color;
}

import 'dart:typed_data';

import 'package:flutter/material.dart';

/// Waddle's default palette. Wallpaper roles replace these through the host.
ColorScheme waddleScheme(Brightness brightness) {
  final dark = brightness == Brightness.dark;
  return ColorScheme.fromSeed(
    seedColor: const Color(0xFF0C7A6F),
    brightness: brightness,
  ).copyWith(
    primary: dark ? const Color(0xFF86D9C6) : const Color(0xFF0C7A6F),
    onPrimary: dark ? const Color(0xFF063B31) : Colors.white,
    primaryContainer: dark ? const Color(0xFF174C40) : const Color(0xFFD5EDE2),
    onPrimaryContainer: dark
        ? const Color(0xFFB7F2DF)
        : const Color(0xFF123E34),
    secondary: dark ? const Color(0xFFCEC3EA) : const Color(0xFF665780),
    onSecondary: dark ? const Color(0xFF342B43) : Colors.white,
    secondaryContainer: dark
        ? const Color(0xFF393147)
        : const Color(0xFFEDE6F6),
    onSecondaryContainer: dark
        ? const Color(0xFFEDE2FF)
        : const Color(0xFF453650),
    tertiary: dark ? const Color(0xFFF4B399) : const Color(0xFF954A30),
    onTertiary: dark ? const Color(0xFF52210F) : Colors.white,
    tertiaryContainer: dark ? const Color(0xFF4C3329) : const Color(0xFFFFE4D5),
    onTertiaryContainer: dark
        ? const Color(0xFFFFDCCC)
        : const Color(0xFF713822),
    surface: dark ? const Color(0xFF111B18) : const Color(0xFFF7F6F0),
    onSurface: dark ? const Color(0xFFEBF0E9) : const Color(0xFF132723),
    onSurfaceVariant: dark ? const Color(0xFFABBEB3) : const Color(0xFF53665C),
    surfaceDim: dark ? const Color(0xFF0D1512) : const Color(0xFFE4E7DD),
    surfaceBright: dark ? const Color(0xFF35413A) : const Color(0xFFFFFEF9),
    surfaceContainerLowest: dark ? const Color(0xFF0C1511) : Colors.white,
    surfaceContainerLow: dark
        ? const Color(0xFF17231D)
        : const Color(0xFFF1F2EA),
    surfaceContainer: dark ? const Color(0xFF1C2A23) : const Color(0xFFECEFE6),
    surfaceContainerHigh: dark
        ? const Color(0xFF24342C)
        : const Color(0xFFE5EAE0),
    surfaceContainerHighest: dark
        ? const Color(0xFF2D3E34)
        : const Color(0xFFDFE6DA),
    outline: dark ? const Color(0xFF81958A) : const Color(0xFF74857B),
    outlineVariant: dark ? const Color(0xFF34473C) : const Color(0xFFDCE3D7),
  );
}

ThemeData waddleTheme(ColorScheme scheme) {
  final base = ThemeData(useMaterial3: true, colorScheme: scheme);
  const heading = FontWeight.w800;
  final text = base.textTheme
      .apply(fontFamily: 'Manrope')
      .copyWith(
        displayLarge: TextStyle(
          fontFamily: 'Manrope',
          fontSize: 60,
          height: 1.06,
          fontWeight: heading,
          letterSpacing: -2.5,
          color: scheme.onSurface,
        ),
        displayMedium: TextStyle(
          fontFamily: 'Manrope',
          fontSize: 46,
          height: 1.08,
          fontWeight: heading,
          letterSpacing: -1.9,
          color: scheme.onSurface,
        ),
        displaySmall: TextStyle(
          fontFamily: 'Manrope',
          fontSize: 38,
          height: 1.12,
          fontWeight: heading,
          letterSpacing: -1.5,
          color: scheme.onSurface,
        ),
        headlineLarge: TextStyle(
          fontFamily: 'Manrope',
          fontSize: 32,
          height: 1.15,
          fontWeight: heading,
          letterSpacing: -1.1,
          color: scheme.onSurface,
        ),
        headlineMedium: TextStyle(
          fontFamily: 'Manrope',
          fontSize: 28,
          height: 1.2,
          fontWeight: heading,
          letterSpacing: -.9,
          color: scheme.onSurface,
        ),
        headlineSmall: TextStyle(
          fontFamily: 'Manrope',
          fontSize: 24,
          height: 1.25,
          fontWeight: heading,
          letterSpacing: -.7,
          color: scheme.onSurface,
        ),
        titleLarge: TextStyle(
          fontFamily: 'Manrope',
          fontSize: 20,
          height: 1.3,
          fontWeight: FontWeight.w700,
          letterSpacing: -.4,
          color: scheme.onSurface,
        ),
        titleMedium: TextStyle(
          fontFamily: 'Manrope',
          fontSize: 16,
          height: 1.4,
          fontWeight: FontWeight.w700,
          letterSpacing: -.2,
          color: scheme.onSurface,
        ),
        titleSmall: TextStyle(
          fontFamily: 'Manrope',
          fontSize: 14,
          height: 1.4,
          fontWeight: FontWeight.w700,
          color: scheme.onSurface,
        ),
        bodyLarge: TextStyle(
          fontFamily: 'Manrope',
          fontSize: 16,
          height: 1.5,
          fontWeight: FontWeight.w500,
          color: scheme.onSurface,
        ),
        bodyMedium: TextStyle(
          fontFamily: 'Manrope',
          fontSize: 14,
          height: 1.5,
          fontWeight: FontWeight.w500,
          color: scheme.onSurface,
        ),
        bodySmall: TextStyle(
          fontFamily: 'Manrope',
          fontSize: 12,
          height: 1.5,
          fontWeight: FontWeight.w500,
          color: scheme.onSurfaceVariant,
        ),
        labelLarge: TextStyle(
          fontFamily: 'Manrope',
          fontSize: 14,
          height: 1.3,
          fontWeight: FontWeight.w700,
          color: scheme.onSurface,
        ),
        labelMedium: TextStyle(
          fontFamily: 'Manrope',
          fontSize: 12,
          height: 1.3,
          fontWeight: FontWeight.w700,
          color: scheme.onSurfaceVariant,
        ),
        labelSmall: TextStyle(
          fontFamily: 'Manrope',
          fontSize: 11,
          height: 1.4,
          fontWeight: FontWeight.w700,
          letterSpacing: .2,
          color: scheme.onSurfaceVariant,
        ),
      );
  final shape = RoundedRectangleBorder(borderRadius: BorderRadius.circular(18));
  return base.copyWith(
    textTheme: text,
    scaffoldBackgroundColor: scheme.surface,
    splashFactory: InkSparkle.splashFactory,
    appBarTheme: AppBarTheme(
      centerTitle: false,
      toolbarHeight: 72,
      titleSpacing: 20,
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
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(24)),
    ),
    inputDecorationTheme: InputDecorationTheme(
      filled: true,
      fillColor: scheme.surfaceContainerLow,
      hintStyle: text.bodyMedium?.copyWith(color: scheme.onSurfaceVariant),
      contentPadding: const EdgeInsets.symmetric(horizontal: 18, vertical: 16),
      border: OutlineInputBorder(
        borderRadius: BorderRadius.circular(18),
        borderSide: BorderSide(color: scheme.outlineVariant),
      ),
      enabledBorder: OutlineInputBorder(
        borderRadius: BorderRadius.circular(18),
        borderSide: BorderSide(color: scheme.outlineVariant),
      ),
      focusedBorder: OutlineInputBorder(
        borderRadius: BorderRadius.circular(18),
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
      style: FilledButton.styleFrom(
        minimumSize: const Size(48, 52),
        padding: const EdgeInsets.symmetric(horizontal: 22, vertical: 14),
        textStyle: text.labelLarge,
        shape: shape,
      ),
    ),
    outlinedButtonTheme: OutlinedButtonThemeData(
      style: OutlinedButton.styleFrom(
        minimumSize: const Size(48, 50),
        padding: const EdgeInsets.symmetric(horizontal: 20, vertical: 12),
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
      height: 76,
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
      contentPadding: const EdgeInsets.symmetric(horizontal: 20, vertical: 6),
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
            fontFamily: 'Fredoka',
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
    final index = name.runes.fold(0, (sum, rune) => sum + rune) % 3;
    final pairs = [
      (scheme.primaryContainer, scheme.onPrimaryContainer),
      (scheme.secondaryContainer, scheme.onSecondaryContainer),
      (scheme.tertiaryContainer, scheme.onTertiaryContainer),
    ];
    final (background, foreground) = pairs[index];
    return Semantics(
      label: '$name${online ? ', online' : ''}',
      child: SizedBox.square(
        dimension: size,
        child: Stack(
          clipBehavior: Clip.none,
          children: [
            ClipRRect(
              borderRadius: BorderRadius.circular(size * .36),
              child: ColoredBox(
                color: background,
                child: SizedBox.expand(
                  child: bytes == null
                      ? Center(
                          child: Text(
                            name.trim().isEmpty
                                ? '?'
                                : name.trim().characters.first.toUpperCase(),
                            style: TextStyle(
                              fontFamily: 'Manrope',
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
                    color: scheme.primary,
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
    borderRadius: BorderRadius.circular(26),
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

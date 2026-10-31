package social.waddle.android.flutter

import android.content.Context
import androidx.compose.material3.ColorScheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.toArgb

/** Material You comes from Android's wallpaper palette without a second Android plugin stack. */
internal fun flutterPalette(context: Context): Map<String, Map<String, Int>> =
    mapOf(
        "light" to paletteSnapshot(dynamicLightColorScheme(context)),
        "dark" to paletteSnapshot(dynamicDarkColorScheme(context)),
    )

private fun paletteSnapshot(scheme: ColorScheme): Map<String, Int> =
    mapOf(
        "primary" to scheme.primary.toArgb(),
        "onPrimary" to scheme.onPrimary.toArgb(),
        "primaryContainer" to scheme.primaryContainer.toArgb(),
        "onPrimaryContainer" to scheme.onPrimaryContainer.toArgb(),
        "secondary" to scheme.secondary.toArgb(),
        "onSecondary" to scheme.onSecondary.toArgb(),
        "secondaryContainer" to scheme.secondaryContainer.toArgb(),
        "onSecondaryContainer" to scheme.onSecondaryContainer.toArgb(),
        "tertiary" to scheme.tertiary.toArgb(),
        "onTertiary" to scheme.onTertiary.toArgb(),
        "tertiaryContainer" to scheme.tertiaryContainer.toArgb(),
        "onTertiaryContainer" to scheme.onTertiaryContainer.toArgb(),
        "error" to scheme.error.toArgb(),
        "onError" to scheme.onError.toArgb(),
        "errorContainer" to scheme.errorContainer.toArgb(),
        "onErrorContainer" to scheme.onErrorContainer.toArgb(),
        "surface" to scheme.surface.toArgb(),
        "onSurface" to scheme.onSurface.toArgb(),
        "onSurfaceVariant" to scheme.onSurfaceVariant.toArgb(),
        "surfaceDim" to scheme.surfaceDim.toArgb(),
        "surfaceBright" to scheme.surfaceBright.toArgb(),
        "surfaceContainerLowest" to scheme.surfaceContainerLowest.toArgb(),
        "surfaceContainerLow" to scheme.surfaceContainerLow.toArgb(),
        "surfaceContainer" to scheme.surfaceContainer.toArgb(),
        "surfaceContainerHigh" to scheme.surfaceContainerHigh.toArgb(),
        "surfaceContainerHighest" to scheme.surfaceContainerHighest.toArgb(),
        "inverseSurface" to scheme.inverseSurface.toArgb(),
        "onInverseSurface" to scheme.inverseOnSurface.toArgb(),
        "inversePrimary" to scheme.inversePrimary.toArgb(),
        "outline" to scheme.outline.toArgb(),
        "outlineVariant" to scheme.outlineVariant.toArgb(),
        "scrim" to scheme.scrim.toArgb(),
        "shadow" to Color.Black.toArgb(),
        "surfaceTint" to scheme.surfaceTint.toArgb(),
        "primaryFixed" to scheme.primaryFixed.toArgb(),
        "primaryFixedDim" to scheme.primaryFixedDim.toArgb(),
        "onPrimaryFixed" to scheme.onPrimaryFixed.toArgb(),
        "onPrimaryFixedVariant" to scheme.onPrimaryFixedVariant.toArgb(),
        "secondaryFixed" to scheme.secondaryFixed.toArgb(),
        "secondaryFixedDim" to scheme.secondaryFixedDim.toArgb(),
        "onSecondaryFixed" to scheme.onSecondaryFixed.toArgb(),
        "onSecondaryFixedVariant" to scheme.onSecondaryFixedVariant.toArgb(),
        "tertiaryFixed" to scheme.tertiaryFixed.toArgb(),
        "tertiaryFixedDim" to scheme.tertiaryFixedDim.toArgb(),
        "onTertiaryFixed" to scheme.onTertiaryFixed.toArgb(),
        "onTertiaryFixedVariant" to scheme.onTertiaryFixedVariant.toArgb(),
    )

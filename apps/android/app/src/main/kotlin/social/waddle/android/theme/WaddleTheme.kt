package social.waddle.android.theme

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.ColorScheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Shapes
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.ReadOnlyComposable
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.compositeOver
import androidx.compose.ui.unit.dp
import social.waddle.android.client.prefs.ThemeMode

/**
 * Waddle's semantic colours that Material 3 has no role for. Everything
 * here comes from [WaddleTokens]; the derived tints are the same
 * `color-mix` recipes the web stylesheet uses.
 */
@Immutable
data class WaddleColors(
    val isDark: Boolean,
    /** Ember: what is happening now — mentions, live calls, speakers. */
    val live: Color,
    val liveText: Color,
    /** Row tint behind a message that mentions the reader. */
    val mentionTint: Color,
    val success: Color,
    val warning: Color,
    val gold: Color,
    val goldText: Color,
    val moss: Color,
    /** Readable error copy (the fill is `colorScheme.error`). */
    val dangerText: Color,
    val codeBackground: Color,
    val codeBorder: Color,
    val codeForeground: Color,
    val quoteBorder: Color,
    val quoteForeground: Color,
    val link: Color,
    /** Hairline separators between regions; quieter than `outline`. */
    val hairline: Color,
    /** Fill for neutral counters (unread without a mention). */
    val neutralBadge: Color,
    val onNeutralBadge: Color,
    /** Accent-tinted selection behind the active list row. */
    val selection: Color,
    /** Theme-independent night ground for the call stage. */
    val nightGround: Color,
    val nightForeground: Color,
    val presenceOnline: Color,
    val presenceAway: Color,
    val presenceBusy: Color,
    val presenceOffline: Color,
)

private fun tint(color: Color, alpha: Float, over: Color): Color = color.copy(alpha = alpha).compositeOver(over)

private object LightPalette {
    private val t = WaddleTokens.Light
    val scheme: ColorScheme = lightColorScheme(
        primary = t.primary,
        onPrimary = t.primaryForeground,
        primaryContainer = t.sidebarAccent,
        onPrimaryContainer = t.foreground,
        inversePrimary = WaddleTokens.Dark.primary,
        secondary = t.mutedForeground,
        onSecondary = t.card,
        secondaryContainer = t.secondary,
        onSecondaryContainer = t.foreground,
        tertiary = t.liveText,
        onTertiary = t.card,
        tertiaryContainer = tint(t.live, alpha = 0.16f, over = t.card),
        onTertiaryContainer = t.liveText,
        error = t.destructive,
        onError = t.destructiveForeground,
        errorContainer = tint(t.destructive, alpha = 0.12f, over = t.card),
        onErrorContainer = t.destructiveText,
        background = t.background,
        onBackground = t.foreground,
        surface = t.background,
        onSurface = t.foreground,
        surfaceVariant = t.secondary,
        onSurfaceVariant = t.mutedForeground,
        surfaceTint = Color.Transparent,
        inverseSurface = t.foreground,
        inverseOnSurface = t.background,
        outline = tint(t.mutedForeground, alpha = 0.55f, over = t.card),
        outlineVariant = t.border,
        scrim = Color.Black,
        surfaceBright = t.card,
        surfaceDim = t.secondary,
        surfaceContainerLowest = t.card,
        surfaceContainerLow = t.card,
        surfaceContainer = t.card,
        surfaceContainerHigh = t.secondary,
        surfaceContainerHighest = t.border,
    )
    val extended = WaddleColors(
        isDark = false,
        live = t.live,
        liveText = t.liveText,
        mentionTint = tint(t.live, alpha = 0.08f, over = t.background),
        success = t.success,
        warning = t.warning,
        gold = t.gold,
        goldText = t.goldText,
        moss = t.moss,
        dangerText = t.destructiveText,
        codeBackground = t.richCodeBg,
        codeBorder = t.richCodeBorder,
        codeForeground = t.richCodeForeground,
        quoteBorder = t.richQuoteBorder,
        quoteForeground = t.richQuoteForeground,
        link = t.richLink,
        hairline = t.border,
        neutralBadge = t.foreground,
        onNeutralBadge = t.card,
        selection = t.sidebarAccent,
        nightGround = t.nightGround,
        nightForeground = t.nightForeground,
        presenceOnline = t.success,
        presenceAway = t.warning,
        presenceBusy = t.destructive,
        presenceOffline = tint(t.mutedForeground, alpha = 0.6f, over = t.card),
    )
}

private object DarkPalette {
    private val t = WaddleTokens.Dark
    val scheme: ColorScheme = darkColorScheme(
        primary = t.primary,
        onPrimary = t.primaryForeground,
        primaryContainer = tint(t.primary, alpha = 0.18f, over = t.card),
        onPrimaryContainer = t.foreground,
        inversePrimary = WaddleTokens.Light.primary,
        secondary = t.mutedForeground,
        onSecondary = t.background,
        secondaryContainer = t.secondary,
        onSecondaryContainer = t.foreground,
        tertiary = t.liveText,
        onTertiary = t.liveForeground,
        tertiaryContainer = tint(t.live, alpha = 0.18f, over = t.card),
        onTertiaryContainer = t.liveText,
        error = t.destructive,
        onError = t.destructiveForeground,
        errorContainer = tint(t.destructive, alpha = 0.2f, over = t.card),
        onErrorContainer = t.destructiveText,
        background = t.background,
        onBackground = t.foreground,
        surface = t.background,
        onSurface = t.foreground,
        surfaceVariant = t.secondary,
        onSurfaceVariant = t.mutedForeground,
        surfaceTint = Color.Transparent,
        inverseSurface = t.foreground,
        inverseOnSurface = t.background,
        outline = tint(t.mutedForeground, alpha = 0.5f, over = t.card),
        outlineVariant = t.border,
        scrim = Color.Black,
        surfaceBright = t.surfaceStrong,
        surfaceDim = t.background,
        surfaceContainerLowest = t.background,
        surfaceContainerLow = t.sidebar,
        surfaceContainer = t.card,
        surfaceContainerHigh = t.secondary,
        surfaceContainerHighest = t.border,
    )
    val extended = WaddleColors(
        isDark = true,
        live = t.live,
        liveText = t.liveText,
        mentionTint = tint(t.live, alpha = 0.1f, over = t.background),
        success = t.success,
        warning = t.warning,
        gold = t.gold,
        goldText = t.goldText,
        moss = t.moss,
        dangerText = t.destructiveText,
        codeBackground = t.richCodeBg,
        codeBorder = t.richCodeBorder,
        codeForeground = t.richCodeForeground,
        quoteBorder = t.richQuoteBorder,
        quoteForeground = t.richQuoteForeground,
        link = t.richLink,
        hairline = t.border,
        neutralBadge = t.secondary,
        onNeutralBadge = t.foreground,
        selection = tint(t.primary, alpha = 0.14f, over = t.sidebar),
        nightGround = t.nightGround,
        nightForeground = t.nightForeground,
        presenceOnline = t.success,
        presenceAway = t.warning,
        presenceBusy = t.destructive,
        presenceOffline = tint(t.mutedForeground, alpha = 0.6f, over = t.card),
    )
}

private val WaddleShapes = Shapes(
    extraSmall = RoundedCornerShape(WaddleTokens.Radius.xs),
    small = RoundedCornerShape(WaddleTokens.Radius.sm),
    medium = RoundedCornerShape(WaddleTokens.Radius.lg),
    large = RoundedCornerShape(16.dp),
    extraLarge = RoundedCornerShape(20.dp),
)

private val LocalWaddleColors = staticCompositionLocalOf { LightPalette.extended }

/** Accessors for the Waddle-specific theme values. */
object Waddle {
    val colors: WaddleColors
        @Composable
        @ReadOnlyComposable
        get() = LocalWaddleColors.current

    /** Spacing scale (4/8/12/16/24/32/48/64). */
    val space = WaddleTokens.Space

    /** Corner radii (6/8/10/12/14). */
    val radius = WaddleTokens.Radius
}

/**
 * Material 3 driven entirely by Waddle's tokens: every colour role in both
 * schemes, the type scale and the corner radii. Dynamic colour stays off
 * (one product across web, iOS and Android); the scheme follows the user
 * preference, which defaults to the system theme.
 */
@Composable
fun WaddleTheme(themeMode: ThemeMode, content: @Composable () -> Unit) {
    val darkTheme = when (themeMode) {
        ThemeMode.LIGHT -> false
        ThemeMode.DARK -> true
        ThemeMode.SYSTEM -> isSystemInDarkTheme()
    }
    val palette = if (darkTheme) DarkPalette.extended else LightPalette.extended
    CompositionLocalProvider(LocalWaddleColors provides palette) {
        MaterialTheme(
            colorScheme = if (darkTheme) DarkPalette.scheme else LightPalette.scheme,
            typography = WaddleTypography,
            shapes = WaddleShapes,
            content = content,
        )
    }
}

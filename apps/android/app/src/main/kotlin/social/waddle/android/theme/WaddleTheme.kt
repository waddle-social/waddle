package social.waddle.android.theme

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.ColorScheme
import androidx.compose.material3.ExperimentalMaterial3ExpressiveApi
import androidx.compose.material3.MaterialExpressiveTheme
import androidx.compose.material3.MotionScheme
import androidx.compose.material3.Shapes
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.ReadOnlyComposable
import androidx.compose.runtime.remember
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.compositeOver
import androidx.compose.ui.platform.LocalContext
import social.waddle.android.client.prefs.ThemeMode

/**
 * Huddle's semantic colours (web `panda.config.ts` semantic tokens), for
 * everything Material 3 has no role for. Values come from
 * [WaddleTokens.Huddle]; the derived tints are the web's `color-mix`
 * recipes.
 *
 * Three rules from the design system decide who may use what:
 * teal ([action]) acts — buttons, links, focus, selection; ember
 * ([live]) is alive — huddles, speakers, mentions — and only live things
 * may glow ([glowLive]); night is home.
 */
@Immutable
data class WaddleColors(
    val isDark: Boolean,
    /** The ground behind every screen. */
    val bg: Color,
    /** Rails and bars: the tab bar, the people strip. */
    val bgRail: Color,
    /** Cards, sheets, dialogs, menus. */
    val surface: Color,
    /** Raised or active: pressed rows, the selected segment. */
    val surface2: Color,
    val surfaceCode: Color,
    val border: Color,
    /** Hairline separators between regions; quieter than [border]. */
    val borderSoft: Color,
    val fg: Color,
    val fgMuted: Color,
    /** Placeholder and tertiary text; not for anything that must be read. */
    val fgSoft: Color,
    val action: Color,
    val actionFg: Color,
    val actionText: Color,
    val actionSubtle: Color,
    /** Ember: what is happening right now. */
    val live: Color,
    val liveFg: Color,
    val liveText: Color,
    val liveSubtle: Color,
    val gold: Color,
    val goldText: Color,
    val goldSubtle: Color,
    val moss: Color,
    val mossText: Color,
    val warning: Color,
    val warningText: Color,
    val danger: Color,
    val dangerFg: Color,
    /** Readable destructive copy; [danger] is only a fill. */
    val dangerText: Color,
    val presenceAvailable: Color,
    val presenceAway: Color,
    val presenceDnd: Color,
    val presenceOffline: Color,
    /** The only glows allowed: around live things, and the action focus. */
    val glowLive: Color,
    val glowAction: Color,
    /** Toasts and tooltips stay night in daylight: live things are photographed at night. */
    val toastBg: Color,
    val toastFg: Color,
    val toastMuted: Color,
    /** Theme-independent night ground for the call stage. */
    val nightGround: Color,
    val nightForeground: Color,
    /** Row tint behind a message that mentions the reader (ember at 8%). */
    val mentionTint: Color,
    val codeBorder: Color,
    val codeForeground: Color,
    val quoteBorder: Color,
    val quoteForeground: Color,
) {
    // Older names kept as views onto the Huddle roles above.
    val hairline: Color get() = borderSoft
    val codeBackground: Color get() = surfaceCode
    val link: Color get() = actionText
    val selection: Color get() = actionSubtle
    val success: Color get() = presenceAvailable

    /** Neutral count fill: daylight's `surface.2` is the ground itself, so it uses the border tone. */
    val neutralBadge: Color get() = if (isDark) surface2 else border

    val onNeutralBadge: Color get() = fg
}

private fun tint(color: Color, alpha: Float, over: Color): Color = color.copy(alpha = alpha).compositeOver(over)

private const val MENTION_TINT_ALPHA = 0.08f
private const val QUOTE_BORDER_ALPHA = 0.5f
private const val ERROR_CONTAINER_ALPHA = 0.14f

private object NightPalette {
    private val h = WaddleTokens.Huddle.Night
    val extended = WaddleColors(
        isDark = true,
        bg = h.bg,
        bgRail = h.bgRail,
        surface = h.surface,
        surface2 = h.surface2,
        surfaceCode = h.surfaceCode,
        border = h.border,
        borderSoft = h.borderSoft,
        fg = h.fg,
        fgMuted = h.fgMuted,
        fgSoft = h.fgSoft,
        action = h.action,
        actionFg = h.actionFg,
        actionText = h.actionText,
        actionSubtle = h.actionSubtle,
        live = h.live,
        liveFg = h.liveFg,
        liveText = h.liveText,
        liveSubtle = h.liveSubtle,
        gold = h.gold,
        goldText = h.goldText,
        goldSubtle = h.goldSubtle,
        moss = h.moss,
        mossText = h.mossText,
        warning = h.warning,
        warningText = h.warningText,
        danger = h.danger,
        dangerFg = h.dangerFg,
        dangerText = h.dangerText,
        presenceAvailable = h.presenceAvailable,
        presenceAway = h.presenceAway,
        presenceDnd = h.presenceDnd,
        presenceOffline = h.presenceOffline,
        glowLive = h.glowLive,
        glowAction = h.glowAction,
        toastBg = h.toastBg,
        toastFg = h.toastFg,
        toastMuted = h.toastMuted,
        nightGround = h.bg,
        nightForeground = h.fg,
        mentionTint = tint(h.live, MENTION_TINT_ALPHA, over = h.bg),
        codeBorder = h.borderSoft,
        codeForeground = WaddleTokens.Dark.richCodeForeground,
        quoteBorder = tint(h.action, QUOTE_BORDER_ALPHA, over = h.bg),
        quoteForeground = h.fgMuted,
    )
    val scheme: ColorScheme = schemeOf(extended)
}

private object DaylightPalette {
    private val h = WaddleTokens.Huddle.Daylight
    val extended = WaddleColors(
        isDark = false,
        bg = h.bg,
        bgRail = h.bgRail,
        surface = h.surface,
        surface2 = h.surface2,
        surfaceCode = h.surfaceCode,
        border = h.border,
        borderSoft = h.borderSoft,
        fg = h.fg,
        fgMuted = h.fgMuted,
        fgSoft = h.fgSoft,
        action = h.action,
        actionFg = h.actionFg,
        actionText = h.actionText,
        actionSubtle = h.actionSubtle,
        live = h.live,
        liveFg = h.liveFg,
        liveText = h.liveText,
        liveSubtle = h.liveSubtle,
        gold = h.gold,
        goldText = h.goldText,
        goldSubtle = h.goldSubtle,
        moss = h.moss,
        mossText = h.mossText,
        warning = h.warning,
        warningText = h.warningText,
        danger = h.danger,
        dangerFg = h.dangerFg,
        dangerText = h.dangerText,
        presenceAvailable = h.presenceAvailable,
        presenceAway = h.presenceAway,
        presenceDnd = h.presenceDnd,
        presenceOffline = h.presenceOffline,
        glowLive = h.glowLive,
        glowAction = h.glowAction,
        toastBg = h.toastBg,
        toastFg = h.toastFg,
        toastMuted = h.toastMuted,
        nightGround = WaddleTokens.Huddle.Night.bg,
        nightForeground = WaddleTokens.Huddle.Night.fg,
        mentionTint = tint(h.live, MENTION_TINT_ALPHA, over = h.bg),
        codeBorder = h.border,
        codeForeground = h.fg,
        quoteBorder = tint(h.action, QUOTE_BORDER_ALPHA, over = h.bg),
        quoteForeground = h.fgMuted,
    )
    val scheme: ColorScheme = schemeOf(extended)
}

/**
 * Material roles onto Huddle roles. Screens sit on [WaddleColors.bg]
 * (`background`); `surface` and every surface container are the card
 * colour, so default sheets, dialogs, menus and list rows read as one
 * raised plane instead of banding against the ground. `tertiary` is teal,
 * not ember, so no stock component turns live by accident.
 */
private fun schemeOf(c: WaddleColors): ColorScheme =
    (if (c.isDark) darkColorScheme() else lightColorScheme()).copy(
        primary = c.action,
        onPrimary = c.actionFg,
        primaryContainer = c.actionSubtle,
        onPrimaryContainer = c.fg,
        inversePrimary = c.actionText,
        secondary = c.fgMuted,
        onSecondary = c.bg,
        secondaryContainer = c.surface2,
        onSecondaryContainer = c.fg,
        tertiary = c.actionText,
        onTertiary = c.actionFg,
        tertiaryContainer = c.actionSubtle,
        onTertiaryContainer = c.fg,
        background = c.bg,
        onBackground = c.fg,
        surface = c.surface,
        onSurface = c.fg,
        surfaceVariant = c.surface2,
        onSurfaceVariant = c.fgMuted,
        surfaceTint = Color.Transparent,
        inverseSurface = c.toastBg,
        inverseOnSurface = c.toastFg,
        error = c.danger,
        onError = c.dangerFg,
        errorContainer = tint(c.danger, ERROR_CONTAINER_ALPHA, over = c.surface),
        onErrorContainer = c.dangerText,
        outline = c.fgSoft,
        outlineVariant = c.border,
        scrim = Color.Black,
        surfaceBright = c.surface2,
        surfaceContainer = c.surface,
        surfaceContainerHigh = c.surface2,
        surfaceContainerHighest = c.surface2,
        surfaceContainerLow = c.surface,
        surfaceContainerLowest = c.bg,
        surfaceDim = c.bg,
    )

private val R = WaddleTokens.Huddle.Radius

/**
 * Huddle radii (tag 4 · control 10 · panel 12 · card 16 · pill): fields
 * and small controls at 10, menus at 12, cards, sheets and dialogs at 16.
 * Buttons keep Material's pill.
 */
private val WaddleShapes = Shapes(
    extraSmall = RoundedCornerShape(R.control),
    small = RoundedCornerShape(R.control),
    medium = RoundedCornerShape(R.panel),
    large = RoundedCornerShape(R.card),
    extraLarge = RoundedCornerShape(R.card),
)

private val LocalWaddleColors = staticCompositionLocalOf { NightPalette.extended }

/** Accessors for the Waddle-specific theme values. */
object Waddle {
    val colors: WaddleColors
        @Composable
        @ReadOnlyComposable
        get() = LocalWaddleColors.current

    /** Spacing scale (4/8/12/16/24/32/48/64). */
    val space = WaddleTokens.Space

    /** Huddle radii: tag 4, control 10, panel 12, card 16, pill. */
    val radius = WaddleTokens.Huddle.Radius

    /** Control heights: xs 28, sm 32, md 36, lg 40, touch 44. */
    val control = WaddleTokens.Huddle.ControlHeight

    /** Huddle text styles beyond the Material roles (kicker, time, stat…). */
    val type = WaddleType
}

/**
 * Material You, Huddle's way: the wallpaper's dynamic palette takes over
 * the action and surface roles (buttons, links, selection, grounds,
 * cards, borders, text tones). Live, presence, danger and recognition
 * colours are meanings, not decoration, so they stay Huddle's.
 */
internal fun WaddleColors.withWallpaper(m: ColorScheme): WaddleColors = copy(
    bg = m.surface,
    bgRail = m.surfaceContainerLow,
    surface = m.surfaceContainer,
    surface2 = m.surfaceContainerHigh,
    surfaceCode = m.surfaceContainerLowest,
    border = m.outlineVariant,
    borderSoft = m.outlineVariant,
    fg = m.onSurface,
    fgMuted = m.onSurfaceVariant,
    fgSoft = m.outline,
    action = m.primary,
    actionFg = m.onPrimary,
    actionText = m.primary,
    actionSubtle = m.primaryContainer,
    glowAction = m.primary.copy(alpha = ACTION_GLOW_ALPHA),
    mentionTint = tint(live, MENTION_TINT_ALPHA, over = m.surface),
    codeBorder = m.outlineVariant,
    codeForeground = m.onSurface,
    quoteBorder = tint(m.primary, QUOTE_BORDER_ALPHA, over = m.surface),
    quoteForeground = m.onSurfaceVariant,
)

private const val ACTION_GLOW_ALPHA = 0.45f

/**
 * Material 3 Expressive driven by Huddle's tokens: every colour role in
 * both themes, the type scale, the radii and the expressive motion
 * scheme. [ThemeMode.SYSTEM] follows the phone's dark mode. [wallpaperColors] is Material You's
 * dynamic colour, opt-in (see [withWallpaper]).
 */
@OptIn(ExperimentalMaterial3ExpressiveApi::class)
@Composable
fun WaddleTheme(
    themeMode: ThemeMode,
    wallpaperColors: Boolean = false,
    content: @Composable () -> Unit,
) {
    val night = themeMode.isNight(isSystemInDarkTheme())
    val huddle = if (night) NightPalette.extended else DaylightPalette.extended
    val context = LocalContext.current
    val palette = if (wallpaperColors) {
        remember(night, context) {
            huddle.withWallpaper(if (night) dynamicDarkColorScheme(context) else dynamicLightColorScheme(context))
        }
    } else {
        huddle
    }
    val scheme = when {
        wallpaperColors -> remember(palette) { schemeOf(palette) }
        night -> NightPalette.scheme
        else -> DaylightPalette.scheme
    }
    CompositionLocalProvider(LocalWaddleColors provides palette) {
        MaterialExpressiveTheme(
            colorScheme = scheme,
            motionScheme = MotionScheme.expressive(),
            typography = WaddleTypography,
            shapes = WaddleShapes,
            content = content,
        )
    }
}

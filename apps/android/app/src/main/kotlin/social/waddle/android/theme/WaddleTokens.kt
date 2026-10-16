// GENERATED — do not edit.
// Sources: chat/src/styles/huddle-tokens.ts (Huddle),
//          chat/src/styles/global/tokens.css (everything else)
// Regenerate: bun scripts/generate-design-tokens.mjs
package social.waddle.android.theme

import androidx.compose.animation.core.CubicBezierEasing
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.TextUnit
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.em
import androidx.compose.ui.unit.sp

/** Waddle's semantic design tokens, shared with the web client. */
object WaddleTokens {
    /** Daylight scheme. */
    object Light {
        val background = Color(0xFFF2F6F7)
        val foreground = Color(0xFF0F1A24)
        val card = Color(0xFFFFFFFF)
        val primary = Color(0xFF0C7A6F)
        val primaryForeground = Color(0xFFFFFFFF)
        val secondary = Color(0xFFE9EFF1)
        val muted = Color(0xFFE9EFF1)
        val mutedForeground = Color(0xFF4A5966)
        val destructive = Color(0xFFD1434B)
        val destructiveForeground = Color(0xFFFFFFFF)
        val destructiveText = Color(0xFFB8353D)
        val border = Color(0xFFD9E2E6)
        val sidebar = Color(0xFFFFFFFF)
        val sidebarAccent = Color(0xFFD6F3EF)
        val surface = Color(0xFFFFFFFF)
        val surfaceStrong = Color(0xFFFFFFFF)
        val rail = Color(0xFFFFFFFF)
        val railForeground = Color(0xFF4A5966)
        val railHover = Color(0xFFE9EFF1)
        val success = Color(0xFF1F9D55)
        val warning = Color(0xFFD97706)
        val warningForeground = Color(0xFF8A4D06)
        val live = Color(0xFFFF7F21)
        val liveForeground = Color(0xFF0B1219)
        val liveText = Color(0xFFB04A06)
        val glow = Color(0x2E0B7A6F)
        val gold = Color(0xFFE9B44C)
        val goldText = Color(0xFF8A5A0A)
        val moss = Color(0xFF2F6B3F)
        val richCodeBg = Color(0xFFE9EFF1)
        val richCodeBorder = Color(0xFFD9E2E6)
        val richCodeForeground = Color(0xFF0F1A24)
        val richQuoteBorder = Color(0x800B7A6F)
        val richQuoteForeground = Color(0xFF4A5966)
        val richLink = Color(0xFF0B7A6F)
        val nightGround = Color(0xFF0B1219)
        val nightForeground = Color(0xFFE8EEF2)
    }

    /** Night scheme. */
    object Dark {
        val background = Color(0xFF0B1219)
        val foreground = Color(0xFFE8EEF2)
        val card = Color(0xFF121F2B)
        val primary = Color(0xFF6FD3C7)
        val primaryForeground = Color(0xFF0B1219)
        val secondary = Color(0xFF1A2A3A)
        val muted = Color(0xFF1A2A3A)
        val mutedForeground = Color(0xFF9FB0BD)
        val destructive = Color(0xFFD1434B)
        val destructiveForeground = Color(0xFFFFFFFF)
        val destructiveText = Color(0xFFFF8B90)
        val border = Color(0xFF26384A)
        val sidebar = Color(0xFF0F1A24)
        val sidebarAccent = Color(0xFF1A2A3A)
        val surface = Color(0xFF121F2B)
        val surfaceStrong = Color(0xFF1A2A3A)
        val rail = Color(0xFF0F1A24)
        val railForeground = Color(0xFF9FB0BD)
        val railHover = Color(0xFF1A2A3A)
        val success = Color(0xFF3DDC84)
        val warning = Color(0xFFF2B036)
        val warningForeground = Color(0xFFF2B036)
        val live = Color(0xFFFFA057)
        val liveForeground = Color(0xFF0B1219)
        val liveText = Color(0xFFFFA057)
        val glow = Color(0x386FD3C7)
        val gold = Color(0xFFE9B44C)
        val goldText = Color(0xFFE9B44C)
        val moss = Color(0xFF7FC98F)
        val richCodeBg = Color(0xFF070C12)
        val richCodeBorder = Color(0xFF26384A)
        val richCodeForeground = Color(0xFFC9D4DC)
        val richQuoteBorder = Color(0x806FD3C7)
        val richQuoteForeground = Color(0xFF9FB0BD)
        val richLink = Color(0xFF6FD3C7)
        val nightGround = Color(0xFF0B1219)
        val nightForeground = Color(0xFFE8EEF2)
    }

    object Space {
        val xxs = 4.dp
        val xs = 8.dp
        val sm = 12.dp
        val md = 16.dp
        val lg = 24.dp
        val xl = 32.dp
        val xxl = 48.dp
        val xxxl = 64.dp
    }

    object Radius {
        val xs = 6.dp
        val sm = 8.dp
        val md = 10.dp
        val lg = 12.dp
        val xl = 14.dp
    }

    object Text {
        val caption = 11.sp
        val meta = 12.sp
        val control = 13.sp
        val body = 14.sp
        val message = 15.sp
        val title = 16.sp
        val display = 20.sp
    }

    /** Line height as a multiple of the font size. */
    object Leading {
        const val COMPACT = 1.25f
        const val CONTROL = 1.35f
        const val BODY = 1.5f
        const val MESSAGE = 1.5f
    }

    /**
     * Huddle, the web's design system (chat/panda.config.ts). Night is the
     * default scheme; Daylight is the light one.
     */
    object Huddle {
        /** Night scheme (the default). */
        object Night {
            val bg = Color(0xFF0B1219)
            val bgRail = Color(0xFF0F1A24)
            val fg = Color(0xFFE8EEF2)
            val fgMuted = Color(0xFF9FB0BD)
            val fgSoft = Color(0xFF6B7F92)
            val surface2 = Color(0xFF1A2A3A)
            val surface = Color(0xFF121F2B)
            val surfaceHover = Color(0xFF1A2A3A)
            val surfaceCode = Color(0xFF070C12)
            val border = Color(0xFF26384A)
            val borderSoft = Color(0xFF1F2F3F)
            val action = Color(0xFF6FD3C7)
            val actionHover = Color(0xFFA3E6DE)
            val actionFg = Color(0xFF0B1219)
            val actionText = Color(0xFF6FD3C7)
            val actionSubtle = Color(0xFF123F3A)
            val live = Color(0xFFFFA057)
            val liveHover = Color(0xFFFF9440)
            val liveFg = Color(0xFF0B1219)
            val liveText = Color(0xFFFFA057)
            val liveSubtle = Color(0xFF4A2A12)
            val gold = Color(0xFFE9B44C)
            val goldText = Color(0xFFE9B44C)
            val goldSubtle = Color(0xFF3D3320)
            val moss = Color(0xFF7FC98F)
            val mossFg = Color(0xFF0B1219)
            val mossText = Color(0xFF7FC98F)
            val warning = Color(0xFFF2B036)
            val warningText = Color(0xFFF2B036)
            val danger = Color(0xFFD1434B)
            val dangerHover = Color(0xFFB8353D)
            val dangerFg = Color(0xFFFFFFFF)
            val dangerText = Color(0xFFFF8B90)
            val presenceAvailable = Color(0xFF3DDC84)
            val presenceAway = Color(0xFFF2B036)
            val presenceDnd = Color(0xFFFF8B90)
            val presenceOffline = Color(0xFF6B7F92)
            val glowLive = Color(0x8CFFA057)
            val glowAction = Color(0x736FD3C7)
            val toastBg = Color(0xFF121F2B)
            val toastFg = Color(0xFFE8EEF2)
            val toastMuted = Color(0xFF9FB0BD)
            val avatarFallback = Color(0xFFD6F3EF)
        }

        /** Daylight scheme. */
        object Daylight {
            val bg = Color(0xFFF2F6F7)
            val bgRail = Color(0xFFFFFFFF)
            val fg = Color(0xFF0F1A24)
            val fgMuted = Color(0xFF4A5966)
            val fgSoft = Color(0xFF9FB0BD)
            val surface2 = Color(0xFFF2F6F7)
            val surface = Color(0xFFFFFFFF)
            val surfaceHover = Color(0xFFF2F6F7)
            val surfaceCode = Color(0xFFF2F6F7)
            val border = Color(0xFFD9E2E6)
            val borderSoft = Color(0xFFD9E2E6)
            val action = Color(0xFF0B7A6F)
            val actionHover = Color(0xFF075F57)
            val actionFg = Color(0xFFFFFFFF)
            val actionText = Color(0xFF0B7A6F)
            val actionSubtle = Color(0xFFD6F3EF)
            val live = Color(0xFFFF7F21)
            val liveHover = Color(0xFFFF9440)
            val liveFg = Color(0xFF0B1219)
            val liveText = Color(0xFFB04A06)
            val liveSubtle = Color(0xFFFFE3CD)
            val gold = Color(0xFFE9B44C)
            val goldText = Color(0xFF8A5A0A)
            val goldSubtle = Color(0xFFFDF3DC)
            val moss = Color(0xFF2F6B3F)
            val mossFg = Color(0xFFFFFFFF)
            val mossText = Color(0xFF2F6B3F)
            val warning = Color(0xFFD97706)
            val warningText = Color(0xFF8A4D06)
            val danger = Color(0xFFD1434B)
            val dangerHover = Color(0xFFB8353D)
            val dangerFg = Color(0xFFFFFFFF)
            val dangerText = Color(0xFFB8353D)
            val presenceAvailable = Color(0xFF1F9D55)
            val presenceAway = Color(0xFFD97706)
            val presenceDnd = Color(0xFFD1434B)
            val presenceOffline = Color(0xFF9FB0BD)
            val glowLive = Color(0x8CFFA057)
            val glowAction = Color(0x736FD3C7)
            val toastBg = Color(0xFF121F2B)
            val toastFg = Color(0xFFE8EEF2)
            val toastMuted = Color(0xFF9FB0BD)
            val avatarFallback = Color(0xFFD6F3EF)
        }

        object Radius {
            val tag = 4.dp
            val control = 10.dp
            val panel = 12.dp
            val card = 16.dp
            val pill = 999.dp
        }

        /** Control heights (web `sizes.control.*`). */
        object ControlHeight {
            val xs = 28.dp
            val sm = 32.dp
            val md = 36.dp
            val lg = 40.dp
            val touch = 44.dp
        }

        object FontSize {
            val kicker = 11.sp
            val caption = 12.sp
            val control = 13.sp
            val body = 14.sp
            val message = 15.sp
            val lead = 17.sp
            val title = 18.sp
            val headline = 30.sp
            val display = 38.sp
            val hero = 52.sp
        }

        /** Line height relative to the font size. */
        object LineHeight {
            val tight = 1.em
            val snug = 1.2.em
            val body = 1.5.em
        }

        object LetterSpacing {
            val display = (-0.03).em
            val title = (-0.01).em
            val kicker = 0.08.em
        }

        /** Web `textStyles`; the theme maps [HuddleFont] onto font families. */
        object Type {
            val hero = HuddleTextStyle(
                font = HuddleFont.Display,
                size = 52.sp,
                weight = FontWeight.Bold,
                lineHeight = 1.em,
                letterSpacing = (-0.03).em,
                uppercase = false,
            )
            val display = HuddleTextStyle(
                font = HuddleFont.Display,
                size = 38.sp,
                weight = FontWeight.Bold,
                lineHeight = 1.em,
                letterSpacing = (-0.03).em,
                uppercase = false,
            )
            val headline = HuddleTextStyle(
                font = HuddleFont.Display,
                size = 30.sp,
                weight = FontWeight.Bold,
                lineHeight = 1.em,
                letterSpacing = (-0.03).em,
                uppercase = false,
            )
            val title = HuddleTextStyle(
                font = HuddleFont.Display,
                size = 18.sp,
                weight = FontWeight.SemiBold,
                lineHeight = 1.2.em,
                letterSpacing = (-0.01).em,
                uppercase = false,
            )
            val stat = HuddleTextStyle(
                font = HuddleFont.Display,
                size = 30.sp,
                weight = FontWeight.Bold,
                lineHeight = 1.em,
                letterSpacing = 0.em,
                uppercase = false,
            )
            val lead = HuddleTextStyle(
                font = HuddleFont.Body,
                size = 17.sp,
                weight = FontWeight.Normal,
                lineHeight = 1.5.em,
                letterSpacing = 0.em,
                uppercase = false,
            )
            val body = HuddleTextStyle(
                font = HuddleFont.Body,
                size = 15.sp,
                weight = FontWeight.Normal,
                lineHeight = 1.5.em,
                letterSpacing = 0.em,
                uppercase = false,
            )
            val control = HuddleTextStyle(
                font = HuddleFont.Body,
                size = 14.sp,
                weight = FontWeight.Medium,
                lineHeight = 1.2.em,
                letterSpacing = 0.em,
                uppercase = false,
            )
            val kicker = HuddleTextStyle(
                font = HuddleFont.Mono,
                size = 11.sp,
                weight = FontWeight.Medium,
                lineHeight = TextUnit.Unspecified,
                letterSpacing = 0.08.em,
                uppercase = true,
            )
            val time = HuddleTextStyle(
                font = HuddleFont.Mono,
                size = 12.sp,
                weight = FontWeight.Normal,
                lineHeight = 1.2.em,
                letterSpacing = 0.em,
                uppercase = false,
            )
        }

        object DurationMillis {
            const val FAST = 160
            const val NORMAL = 240
            const val ENTER = 400
        }

        object Easing {
            val out = CubicBezierEasing(0.2f, 0.7f, 0.2f, 1f)
        }

        object Shadow {
            val overlay = HuddleShadow(
                offsetX = 0.dp,
                offsetY = 16.dp,
                blur = 40.dp,
                spread = 0.dp,
                color = Color(0x73000000),
            )
        }
    }
}

/** Huddle font roles (web `fonts` tokens). */
enum class HuddleFont {
    /** Space Grotesk Variable */
    Display,

    /** Outfit Variable */
    Body,

    /** JetBrains Mono Variable */
    Mono,
}

/**
 * A Huddle text style. [lineHeight] and [letterSpacing] are in em;
 * [lineHeight] is unspecified where the web inherits it.
 */
data class HuddleTextStyle(
    val font: HuddleFont,
    val size: TextUnit,
    val weight: FontWeight,
    val lineHeight: TextUnit,
    val letterSpacing: TextUnit,
    val uppercase: Boolean,
)

/** A single-layer box shadow. */
data class HuddleShadow(
    val offsetX: Dp,
    val offsetY: Dp,
    val blur: Dp,
    val spread: Dp,
    val color: Color,
)

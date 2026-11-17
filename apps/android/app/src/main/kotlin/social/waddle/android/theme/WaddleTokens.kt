// GENERATED — do not edit.
// Sources: design/src/preset.ts (Huddle),
//          scripts/design-tokens/legacy-tokens.css (everything else)
// Regenerate: bun scripts/generate-design-tokens.mjs
package social.waddle.android.theme

import androidx.compose.animation.core.CubicBezierEasing
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
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
     * Polar Huddle (design/src/preset.ts). Night is the default scheme;
     * Daylight is the light one.
     */
    object Huddle {
        /** Night scheme (the default). */
        object Night {
            val bg = Color(0xFF060F19)
            val bgRail = Color(0xFF0A131E)
            val surface2 = Color(0xFF1A242F)
            val surface3 = Color(0xFF232E39)
            val surface = Color(0xFF111A25)
            val surfaceCode = Color(0xFF020912)
            val borderSoft = Color(0xFF28333F)
            val border = Color(0xFF3B4653)
            val borderStrong = Color(0xFF9AA2AD)
            val fg = Color(0xFFEDF0F4)
            val fgMuted = Color(0xFFC1C8D0)
            val fgSoft = Color(0xFF9AA2AD)
            val action = Color(0xFFFFC139)
            val actionHover = Color(0xFFFFD27D)
            val actionFg = Color(0xFF060F19)
            val actionText = Color(0xFFFFD27D)
            val actionSubtle = Color(0xFF2B1C00)
            val live = Color(0xFF4ED7DF)
            val liveHover = Color(0xFF85E5EA)
            val liveFg = Color(0xFF060F19)
            val liveText = Color(0xFF85E5EA)
            val liveSubtle = Color(0xFF002021)
            val danger = Color(0xFFD33944)
            val dangerHover = Color(0xFFAA1D2C)
            val dangerFg = Color(0xFFFFFFFF)
            val dangerText = Color(0xFFFFAFAC)
            val dangerSubtle = Color(0xFF360307)
            val warning = Color(0xFFFC924B)
            val warningText = Color(0xFFFFB486)
            val warningSubtle = Color(0xFF2D1000)
            val success = Color(0xFF67C986)
            val successText = Color(0xFF92DBA6)
            val successSubtle = Color(0xFF00210B)
            val info = Color(0xFF78B5F9)
            val infoText = Color(0xFF9ECCFF)
            val infoSubtle = Color(0xFF041A32)
            val focus = Color(0xFF9ECCFF)
            val presenceAvailable = Color(0xFF67C986)
            val presenceAway = Color(0xFFFFC139)
            val presenceDnd = Color(0xFFFF8987)
            val presenceOffline = Color(0xFF9AA2AD)
            val gold = Color(0xFFFFC139)
            val goldFg = Color(0xFF060F19)
            val goldText = Color(0xFFFFD27D)
            val goldSubtle = Color(0xFF2B1C00)
            val halo = Color(0xFFC4DFFF)
            val mist = Color(0xFFC1C8D0)
            val sea = Color(0xFF041A32)
            val seaDeep = Color(0xFF030F1F)
            val seaShallow = Color(0xFF062C50)
            val avatar1 = Color(0xFFFFE0A9)
            val avatar2 = Color(0xFFC4DFFF)
            val avatar3 = Color(0xFFDBE0E6)
            val avatar4 = Color(0xFFB4EEF1)
            val avatar5 = Color(0xFFFFCFCC)
            val avatar6 = Color(0xFFBCEAC7)
            val avatarFg = Color(0xFF060F19)
            val scrim = Color(0xB8020912)
            val selection = Color(0x5278B5F9)
            val belly = Color(0xFFFFFFFF)
            val ink = Color(0xFF14181E)
            val beak = Color(0xFFFF8A3D)
        }

        /** Daylight scheme. */
        object Daylight {
            val bg = Color(0xFFF8FAFD)
            val bgRail = Color(0xFFFFFFFF)
            val surface2 = Color(0xFFEDF0F4)
            val surface3 = Color(0xFFFFFFFF)
            val surface = Color(0xFFFFFFFF)
            val surfaceCode = Color(0xFFEDF0F4)
            val borderSoft = Color(0xFFDBE0E6)
            val border = Color(0xFFC8CED6)
            val borderStrong = Color(0xFF737D88)
            val fg = Color(0xFF060F19)
            val fgMuted = Color(0xFF38424E)
            val fgSoft = Color(0xFF525C67)
            val action = Color(0xFF0F4376)
            val actionHover = Color(0xFF062C50)
            val actionFg = Color(0xFFFFFFFF)
            val actionText = Color(0xFF6B4C00)
            val actionSubtle = Color(0xFFFFEED0)
            val live = Color(0xFF007176)
            val liveHover = Color(0xFF005256)
            val liveFg = Color(0xFFFFFFFF)
            val liveText = Color(0xFF005256)
            val liveSubtle = Color(0xFFEBFAFB)
            val danger = Color(0xFFD33944)
            val dangerHover = Color(0xFFAA1D2C)
            val dangerFg = Color(0xFFFFFFFF)
            val dangerText = Color(0xFFAA1D2C)
            val dangerSubtle = Color(0xFFFFF4F3)
            val warning = Color(0xFFE77412)
            val warningText = Color(0xFF924500)
            val warningSubtle = Color(0xFFFFF4EE)
            val success = Color(0xFF0A924B)
            val successText = Color(0xFF017137)
            val successSubtle = Color(0xFFEEFBF1)
            val info = Color(0xFF397DC4)
            val infoText = Color(0xFF205F9F)
            val infoSubtle = Color(0xFFF1F8FF)
            val focus = Color(0xFF397DC4)
            val presenceAvailable = Color(0xFF0A924B)
            val presenceAway = Color(0xFFBB8800)
            val presenceDnd = Color(0xFFD33944)
            val presenceOffline = Color(0xFF737D88)
            val gold = Color(0xFFFFC139)
            val goldFg = Color(0xFF060F19)
            val goldText = Color(0xFF6B4C00)
            val goldSubtle = Color(0xFFFFEED0)
            val halo = Color(0xFF205F9F)
            val mist = Color(0xFF38424E)
            val sea = Color(0xFFF1F8FF)
            val seaDeep = Color(0xFFE0EFFF)
            val seaShallow = Color(0xFFC4DFFF)
            val avatar1 = Color(0xFFFFE0A9)
            val avatar2 = Color(0xFFC4DFFF)
            val avatar3 = Color(0xFFDBE0E6)
            val avatar4 = Color(0xFFB4EEF1)
            val avatar5 = Color(0xFFFFCFCC)
            val avatar6 = Color(0xFFBCEAC7)
            val avatarFg = Color(0xFF060F19)
            val scrim = Color(0x80060F19)
            val selection = Color(0x669ECCFF)
            val belly = Color(0xFFFFFFFF)
            val ink = Color(0xFF14181E)
            val beak = Color(0xFFFF8A3D)
        }

        object Radius {
            val xs = 4.dp
            val sm = 8.dp
            val tag = 6.dp
            val control = 12.dp
            val panel = 16.dp
            val card = 20.dp
            val bubble = 20.dp
            val sheet = 28.dp
            val pill = 999.dp
        }

        /** Avatar fallback tints (web `avatar.N`), one picked per name; same in both schemes. */
        val avatarTints = listOf(
            Color(0xFFFFE0A9),
            Color(0xFFC4DFFF),
            Color(0xFFDBE0E6),
            Color(0xFFB4EEF1),
            Color(0xFFFFCFCC),
            Color(0xFFBCEAC7),
        )

        /** Control heights (comfortable density; touch is the 44dp target). */
        object ControlHeight {
            val xs = 28.dp
            val sm = 32.dp
            val md = 36.dp
            val lg = 40.dp
            val touch = 44.dp
        }

        object FontSize {
            val xxs = 12.sp
            val xs = 13.sp
            val sm = 14.sp
            val md = 15.sp
            val base = 16.sp
            val lg = 18.sp
            val xl = 20.sp
            val xxl = 24.sp
            val xxxl = 30.sp
            val xxxxl = 40.sp
            val xxxxxl = 60.sp
        }

        /** Web `textStyles`; the theme maps [HuddleFont] onto font families. */
        object Type {
            val hero = HuddleTextStyle(
                font = HuddleFont.Display,
                size = 60.sp,
                weight = FontWeight(800),
                lineHeight = 1.em,
                letterSpacing = (-0.03).em,
            )
            val display = HuddleTextStyle(
                font = HuddleFont.Display,
                size = 40.sp,
                weight = FontWeight(800),
                lineHeight = 1.1.em,
                letterSpacing = (-0.025).em,
            )
            val headline = HuddleTextStyle(
                font = HuddleFont.Display,
                size = 30.sp,
                weight = FontWeight(750),
                lineHeight = 1.2.em,
                letterSpacing = (-0.02).em,
            )
            val title = HuddleTextStyle(
                font = HuddleFont.Display,
                size = 20.sp,
                weight = FontWeight(700),
                lineHeight = 1.3.em,
                letterSpacing = (-0.01).em,
            )
            val subtitle = HuddleTextStyle(
                font = HuddleFont.Display,
                size = 18.sp,
                weight = FontWeight(700),
                lineHeight = 1.333.em,
                letterSpacing = (-0.01).em,
            )
            val body = HuddleTextStyle(
                font = HuddleFont.Body,
                size = 15.sp,
                weight = FontWeight(400),
                lineHeight = 1.467.em,
                letterSpacing = 0.em,
            )
            val message = HuddleTextStyle(
                font = HuddleFont.Body,
                size = 16.sp,
                weight = FontWeight(400),
                lineHeight = 1.5.em,
                letterSpacing = 0.em,
            )
            val author = HuddleTextStyle(
                font = HuddleFont.Body,
                size = 15.sp,
                weight = FontWeight(700),
                lineHeight = 1.333.em,
                letterSpacing = 0.em,
            )
            val timestamp = HuddleTextStyle(
                font = HuddleFont.Body,
                size = 13.sp,
                weight = FontWeight(400),
                lineHeight = 1.385.em,
                letterSpacing = 0.em,
            )
            val channel = HuddleTextStyle(
                font = HuddleFont.Display,
                size = 18.sp,
                weight = FontWeight(700),
                lineHeight = 1.333.em,
                letterSpacing = 0.em,
            )
            val sidebar = HuddleTextStyle(
                font = HuddleFont.Body,
                size = 15.sp,
                weight = FontWeight(500),
                lineHeight = 1.333.em,
                letterSpacing = 0.em,
            )
            val sidebarSection = HuddleTextStyle(
                font = HuddleFont.Body,
                size = 13.sp,
                weight = FontWeight(700),
                lineHeight = 1.385.em,
                letterSpacing = 0.em,
            )
            val badge = HuddleTextStyle(
                font = HuddleFont.Body,
                size = 12.sp,
                weight = FontWeight(700),
                lineHeight = 1.em,
                letterSpacing = 0.01.em,
            )
            val tag = HuddleTextStyle(
                font = HuddleFont.Body,
                size = 12.sp,
                weight = FontWeight(600),
                lineHeight = 1.em,
                letterSpacing = 0.01.em,
            )
            val button = HuddleTextStyle(
                font = HuddleFont.Body,
                size = 14.sp,
                weight = FontWeight(600),
                lineHeight = 1.em,
                letterSpacing = 0.em,
            )
            val input = HuddleTextStyle(
                font = HuddleFont.Body,
                size = 15.sp,
                weight = FontWeight(400),
                lineHeight = 1.467.em,
                letterSpacing = 0.em,
            )
            val label = HuddleTextStyle(
                font = HuddleFont.Body,
                size = 14.sp,
                weight = FontWeight(600),
                lineHeight = 1.429.em,
                letterSpacing = 0.em,
            )
            val caption = HuddleTextStyle(
                font = HuddleFont.Body,
                size = 13.sp,
                weight = FontWeight(400),
                lineHeight = 1.385.em,
                letterSpacing = 0.005.em,
            )
            val eyebrow = HuddleTextStyle(
                font = HuddleFont.Body,
                size = 13.sp,
                weight = FontWeight(700),
                lineHeight = 1.385.em,
                letterSpacing = 0.01.em,
            )
            val code = HuddleTextStyle(
                font = HuddleFont.Mono,
                size = 14.sp,
                weight = FontWeight(400),
                lineHeight = 1.429.em,
                letterSpacing = 0.em,
            )
        }

        object DurationMillis {
            const val INSTANT = 90
            const val FAST = 150
            const val BASE = 220
            const val SLOW = 320
            const val SLOWER = 480
            const val AMBIENT = 2400
        }

        object Easing {
            val out = CubicBezierEasing(0.2f, 0.7f, 0.2f, 1f)
            val easeIn = CubicBezierEasing(0.4f, 0f, 1f, 1f)
            val inOut = CubicBezierEasing(0.65f, 0f, 0.35f, 1f)
            val spring = CubicBezierEasing(0.34f, 1.56f, 0.64f, 1f)
        }
    }
}

/** Polar Huddle font roles (web `fonts` tokens). */
enum class HuddleFont {
    Display,
    Body,
    Mono,
}

/** A Polar Huddle text style. [lineHeight] and [letterSpacing] are in em. */
data class HuddleTextStyle(
    val font: HuddleFont,
    val size: TextUnit,
    val weight: FontWeight,
    val lineHeight: TextUnit,
    val letterSpacing: TextUnit,
)

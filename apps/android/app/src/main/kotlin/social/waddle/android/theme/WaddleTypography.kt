package social.waddle.android.theme

import androidx.compose.material3.Typography
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.Font
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.LineHeightStyle
import androidx.compose.ui.unit.TextUnit
import androidx.compose.ui.unit.em
import androidx.compose.ui.unit.sp
import social.waddle.android.R

/** Outfit: every UI and message string (web `--font-ui`). */
val OutfitFamily = FontFamily(
    Font(R.font.outfit_regular, FontWeight.Normal),
    Font(R.font.outfit_medium, FontWeight.Medium),
    Font(R.font.outfit_semibold, FontWeight.SemiBold),
    Font(R.font.outfit_bold, FontWeight.Bold),
)

/** Space Grotesk: display and screen titles (web `--font-display`). */
val SpaceGroteskFamily = FontFamily(
    Font(R.font.space_grotesk_medium, FontWeight.Medium),
    Font(R.font.space_grotesk_semibold, FontWeight.SemiBold),
    Font(R.font.space_grotesk_bold, FontWeight.Bold),
)

/** JetBrains Mono: code, timestamps, counts and kickers (web `--font-code`). */
val JetBrainsMonoFamily = FontFamily(
    Font(R.font.jetbrains_mono_regular, FontWeight.Normal),
    Font(R.font.jetbrains_mono_medium, FontWeight.Medium),
)

/** Extra leading split evenly above and below each line, as CSS does. */
private val Centered = LineHeightStyle(
    alignment = LineHeightStyle.Alignment.Center,
    trim = LineHeightStyle.Trim.None,
)

private fun style(
    family: FontFamily,
    size: TextUnit,
    leading: Float,
    weight: FontWeight = FontWeight.Normal,
) = TextStyle(
    fontFamily = family,
    fontWeight = weight,
    fontSize = size,
    lineHeight = (size.value * leading).sp,
    // Web sets `letter-spacing: 0`; Material's 0.5sp body tracking reads
    // as a template on Outfit.
    letterSpacing = 0.em,
    lineHeightStyle = Centered,
)

private val T = WaddleTokens.Text
private val L = WaddleTokens.Leading

/**
 * The web type scale mapped onto Material roles:
 * message 15 · body 14 · control 13 · meta 12 · caption 11 · title 16 ·
 * display 20 (Space Grotesk). Larger display roles keep Space Grotesk.
 */
val WaddleTypography = Typography(
    displayLarge = style(SpaceGroteskFamily, 44.sp, L.COMPACT, FontWeight.Bold),
    displayMedium = style(SpaceGroteskFamily, 36.sp, L.COMPACT, FontWeight.Bold),
    displaySmall = style(SpaceGroteskFamily, 30.sp, L.COMPACT, FontWeight.Bold),
    headlineLarge = style(SpaceGroteskFamily, 28.sp, L.COMPACT, FontWeight.Bold),
    headlineMedium = style(SpaceGroteskFamily, 24.sp, L.COMPACT, FontWeight.Bold),
    headlineSmall = style(SpaceGroteskFamily, 22.sp, L.COMPACT, FontWeight.Medium),
    titleLarge = style(SpaceGroteskFamily, T.display, L.COMPACT, FontWeight.Bold),
    titleMedium = style(OutfitFamily, T.title, L.CONTROL, FontWeight.SemiBold),
    titleSmall = style(OutfitFamily, T.body, L.CONTROL, FontWeight.SemiBold),
    bodyLarge = style(OutfitFamily, T.message, L.MESSAGE),
    bodyMedium = style(OutfitFamily, T.body, L.BODY),
    bodySmall = style(OutfitFamily, T.meta, L.BODY),
    labelLarge = style(OutfitFamily, T.body, L.CONTROL, FontWeight.Medium),
    labelMedium = style(OutfitFamily, T.meta, L.CONTROL, FontWeight.Medium),
    labelSmall = style(OutfitFamily, T.caption, L.CONTROL, FontWeight.Medium),
)

/** Timestamps, counters and kickers: mono at meta size, like the web rail. */
val MonoMeta = style(JetBrainsMonoFamily, T.meta, L.COMPACT)

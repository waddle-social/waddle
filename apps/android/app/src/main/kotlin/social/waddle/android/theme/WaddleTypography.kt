package social.waddle.android.theme

import androidx.compose.material3.Typography
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.LineHeightStyle
import androidx.compose.ui.unit.TextUnit
import androidx.compose.ui.unit.em
import androidx.compose.ui.unit.isSpecified
import androidx.compose.ui.unit.sp

/**
 * Waddle ships no font files on any platform: display and body use the
 * system face (headlines differ by weight and size), mono uses the system
 * monospace.
 */
val MonoFamily: FontFamily = FontFamily.Monospace

/** Extra leading split evenly above and below each line, as CSS does. */
private val Centered = LineHeightStyle(
    alignment = LineHeightStyle.Alignment.Center,
    trim = LineHeightStyle.Trim.None,
)

private fun familyOf(font: HuddleFont): FontFamily = when (font) {
    HuddleFont.Display, HuddleFont.Body -> FontFamily.Default
    HuddleFont.Mono -> MonoFamily
}

/**
 * A Huddle text style as a Compose style. Line height is relative to the
 * size (`em`), as on the web; a style that inherits it on the web gets
 * [fallbackLeading]. Case is not a style in Compose: uppercase styles are
 * rendered through [social.waddle.android.ui.huddle.Kicker].
 */
private fun HuddleTextStyle.toStyle(
    size: TextUnit = this.size,
    weight: FontWeight = this.weight,
    fallbackLeading: Float = SNUG,
): TextStyle {
    val leading = if (lineHeight.isSpecified) lineHeight.value else fallbackLeading
    return TextStyle(
        fontFamily = familyOf(font),
        fontWeight = weight,
        fontSize = size,
        lineHeight = (size.value * leading).sp,
        letterSpacing = letterSpacing,
        lineHeightStyle = Centered,
    )
}

private const val SNUG = 1.2f

/** Display lines get a little air on Android, where 1.0 clips accents in two-line headings. */
private const val DISPLAY_LEADING = 1.1f

private val T = WaddleTokens.Huddle.Type

/**
 * Huddle's text styles (web `textStyles`), for the roles Material has no
 * name for. Prefer these by name in new code: `Waddle.type.kicker`.
 */
object WaddleType {
    val hero: TextStyle = T.hero.toStyle().copy(lineHeight = (T.hero.size.value * DISPLAY_LEADING).sp)
    val display: TextStyle = T.display.toStyle().copy(lineHeight = (T.display.size.value * DISPLAY_LEADING).sp)
    val headline: TextStyle = T.headline.toStyle().copy(lineHeight = (T.headline.size.value * DISPLAY_LEADING).sp)

    /** Room, card, section and people titles: display 18/600. */
    val title: TextStyle = T.title.toStyle()

    /** Big numbers. */
    val stat: TextStyle = T.stat.toStyle()
    val lead: TextStyle = T.lead.toStyle()

    /** Message body: body 15/1.5. */
    val body: TextStyle = T.body.toStyle()

    /** Buttons, tabs, fields: body 14/500. */
    val control: TextStyle = T.control.toStyle()

    /** Mono 11/500, tracked 0.08em; render uppercase through `Kicker`. */
    val kicker: TextStyle = T.kicker.toStyle()

    /** Timestamps and counts: mono 12. */
    val time: TextStyle = T.time.toStyle()
}

private val F = WaddleTokens.Huddle.FontSize

/**
 * Huddle mapped onto Material roles, so stock components follow it:
 * display roles are the hero/display/headline styles, title roles are
 * the display style (top bars, sheet and dialog titles), body roles the body style at
 * message/body/caption sizes, labels the control style down to kicker
 * size.
 */
val WaddleTypography = Typography(
    displayLarge = WaddleType.hero,
    displayMedium = WaddleType.display,
    displaySmall = WaddleType.headline,
    headlineLarge = WaddleType.headline,
    headlineMedium = T.title.toStyle(size = 24.sp, weight = FontWeight.Bold).copy(letterSpacing = (-0.02).em),
    headlineSmall = T.title.toStyle(size = 20.sp, weight = FontWeight.Bold),
    titleLarge = WaddleType.title,
    titleMedium = T.title.toStyle(size = 16.sp),
    titleSmall = T.title.toStyle(size = F.body),
    bodyLarge = WaddleType.body,
    bodyMedium = T.body.toStyle(size = F.body),
    bodySmall = T.body.toStyle(size = F.caption).copy(lineHeight = (F.caption.value * CAPTION_LEADING).sp),
    labelLarge = WaddleType.control,
    labelMedium = T.control.toStyle(size = F.caption),
    labelSmall = T.control.toStyle(size = F.kicker),
)

private const val CAPTION_LEADING = 1.35f

/** Timestamps, counters and kickers: Huddle `time` (mono 12). */
val MonoMeta: TextStyle = WaddleType.time

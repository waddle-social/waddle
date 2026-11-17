package social.waddle.android.theme

import androidx.annotation.FontRes
import androidx.compose.material3.Typography
import androidx.compose.ui.text.ExperimentalTextApi
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.Font
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontVariation
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.LineHeightStyle
import androidx.compose.ui.unit.TextUnit
import androidx.compose.ui.unit.em
import androidx.compose.ui.unit.sp
import social.waddle.android.R

/**
 * One variable font file as a family: an instance per weight the text
 * styles use, each pinned on the `wght` axis.
 */
@OptIn(ExperimentalTextApi::class)
private fun variableFamily(@FontRes font: Int, vararg weights: Int): FontFamily = FontFamily(
    weights.map { weight ->
        Font(font, FontWeight(weight), variationSettings = FontVariation.Settings(FontVariation.weight(weight)))
    },
)

// ponytail: Bricolage's opsz axis stays at its default; set it per size if headlines look off next to the web.
/** Polar Huddle display face: Bricolage Grotesque. */
val DisplayFamily: FontFamily = variableFamily(R.font.bricolage_grotesque, 500, 600, 700, 750, 800)

/** Polar Huddle body face: Atkinson Hyperlegible Next. */
val BodyFamily: FontFamily = variableFamily(R.font.atkinson_hyperlegible_next, 400, 500, 600, 700, 800)

/** Polar Huddle code face: Atkinson Hyperlegible Mono. */
val MonoFamily: FontFamily = variableFamily(R.font.atkinson_hyperlegible_mono, 400, 500, 600, 700)

/** Extra leading split evenly above and below each line, as CSS does. */
private val Centered = LineHeightStyle(
    alignment = LineHeightStyle.Alignment.Center,
    trim = LineHeightStyle.Trim.None,
)

private fun familyOf(font: HuddleFont): FontFamily = when (font) {
    HuddleFont.Display -> DisplayFamily
    HuddleFont.Body -> BodyFamily
    HuddleFont.Mono -> MonoFamily
}

/** A Polar Huddle text style as a Compose style; line height is relative to the size, as on the web. */
private fun HuddleTextStyle.toStyle(
    size: TextUnit = this.size,
    weight: FontWeight = this.weight,
    leading: Float = lineHeight.value,
): TextStyle = TextStyle(
    fontFamily = familyOf(font),
    fontWeight = weight,
    fontSize = size,
    lineHeight = (size.value * leading).sp,
    letterSpacing = letterSpacing,
    lineHeightStyle = Centered,
)

/** Single-line web styles (badge, tag) set leading 1; Compose gets a little room so wrapped text never clips. */
private const val TIGHT_LEADING = 1.25f

private const val LEAD_LEADING = 1.5f

private val T = WaddleTokens.Huddle.Type
private val F = WaddleTokens.Huddle.FontSize

/**
 * Polar Huddle's text styles (web `textStyles`), for the roles Material
 * has no name for. Prefer these by name: `Waddle.type.eyebrow`.
 */
object WaddleType {
    val hero: TextStyle = T.hero.toStyle()
    val display: TextStyle = T.display.toStyle()
    val headline: TextStyle = T.headline.toStyle()

    /** Screen, card and sheet titles: display 20/700. */
    val title: TextStyle = T.title.toStyle()

    /** Section and room names: display 18/700. */
    val subtitle: TextStyle = T.subtitle.toStyle()

    /** Big numbers. */
    val stat: TextStyle = T.headline.toStyle()

    /** Intro copy under a title: body 18/1.5. */
    val lead: TextStyle = T.body.toStyle(size = F.lg, leading = LEAD_LEADING)

    /** Message body: body 16/24. */
    val body: TextStyle = T.message.toStyle()

    /** Secondary copy and row text: body 15/22. */
    val bodySmall: TextStyle = T.body.toStyle()

    /** Message author: body 15/700. */
    val author: TextStyle = T.author.toStyle()

    /** Buttons, tabs, fields and row labels: body 14/600. */
    val control: TextStyle = T.label.toStyle()

    /** Section label above a group, card or count line: body 13/700, sentence case. */
    val eyebrow: TextStyle = T.eyebrow.toStyle()

    /** Timestamps and meta lines: body 13/18. */
    val time: TextStyle = T.timestamp.toStyle()

    /** Counts: body 12/700. */
    val badge: TextStyle = T.badge.toStyle(leading = TIGHT_LEADING)

    /** Roles and states: body 12/600. */
    val tag: TextStyle = T.tag.toStyle(leading = TIGHT_LEADING)

    /** Inline and block code: mono 14/20. */
    val code: TextStyle = T.code.toStyle()
}

/**
 * Polar Huddle mapped onto Material roles, so stock components follow it:
 * display and headline roles are the display face, titles the display face
 * at chat sizes, body roles the message/body/caption styles, labels the
 * label and tag styles.
 */
val WaddleTypography = Typography(
    displayLarge = WaddleType.hero,
    displayMedium = WaddleType.display,
    displaySmall = WaddleType.headline,
    headlineLarge = WaddleType.headline,
    headlineMedium = T.title.toStyle(size = F.xxl, weight = FontWeight(750)).copy(letterSpacing = (-0.015).em),
    headlineSmall = WaddleType.title,
    titleLarge = WaddleType.title,
    titleMedium = T.subtitle.toStyle(size = F.base),
    titleSmall = WaddleType.control,
    bodyLarge = WaddleType.body,
    bodyMedium = WaddleType.bodySmall,
    bodySmall = T.caption.toStyle(),
    labelLarge = WaddleType.control,
    labelMedium = T.label.toStyle(size = F.xs),
    labelSmall = WaddleType.tag,
)

/** Timestamps and counters. */
val MonoMeta: TextStyle = WaddleType.time

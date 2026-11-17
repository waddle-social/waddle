package social.waddle.android.ui.huddle

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.defaultMinSize
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import social.waddle.android.theme.Waddle

/**
 * Polar Huddle's kicker (web `wd-kicker`, the eyebrow style): body 13/700,
 * sentence case, muted — the small label above a section, a card or a
 * count line ("Fri 16 Oct · 12 around"). [live] turns it aurora with a
 * [LiveDot] in front, for what is happening now. Kickers that head a
 * section are headings for screen readers.
 */
@Composable
fun Kicker(
    text: String,
    modifier: Modifier = Modifier,
    live: Boolean = false,
    heading: Boolean = false,
    color: Color = if (live) Waddle.colors.liveText else Waddle.colors.fgMuted,
) {
    Row(
        modifier = modifier.then(if (heading) Modifier.semantics { this.heading() } else Modifier),
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (live) LiveDot()
        Text(
            text = text,
            style = Waddle.type.eyebrow,
            color = color,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

/** What a [CountBadge] counts, which decides its colour. */
enum class CountTone {
    /** Mentions: the gold neck patch, "this is for you". */
    MENTION,

    /** Plain unread: quiet. */
    NEUTRAL,
}

/**
 * A small count (web `wd-badge`): gold for mentions, quiet (surface 2,
 * muted, hairline) for plain unread. Counts above 99 read "99+". [prefix]
 * adds "@" for mention counts.
 */
@Composable
fun CountBadge(
    count: Int,
    modifier: Modifier = Modifier,
    tone: CountTone = CountTone.NEUTRAL,
    prefix: String = "",
) {
    val colors = Waddle.colors
    val (fill, ink) = when (tone) {
        CountTone.MENTION -> colors.gold to colors.goldFg
        CountTone.NEUTRAL -> colors.surface2 to colors.fgMuted
    }
    val shape = RoundedCornerShape(Waddle.radius.pill)
    Box(
        modifier = modifier
            .defaultMinSize(minWidth = BADGE_HEIGHT, minHeight = BADGE_HEIGHT)
            .background(fill, shape)
            .then(if (tone == CountTone.NEUTRAL) Modifier.border(HAIRLINE, colors.border, shape) else Modifier)
            .padding(horizontal = BADGE_PADDING),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            text = prefix + if (count > MAX_COUNT) "$MAX_COUNT+" else count.toString(),
            style = Waddle.type.badge,
            color = ink,
            maxLines = 1,
        )
    }
}

/** Tone of a [HuddleTag]: roles and states (web `tag` recipe). */
enum class TagTone {
    NEUTRAL,

    /** Owner and host. */
    GOLD,

    /** Admin, moderator, helper, verified. */
    ACTION,

    /** Bots. */
    SUCCESS,

    /** Live states: "Live", "Speaking". */
    LIVE,
}

/** A tag for roles and states (web `wd-tag`): 22dp, hairline outline, radius 6, coloured text. */
@Composable
fun HuddleTag(text: String, modifier: Modifier = Modifier, tone: TagTone = TagTone.NEUTRAL) {
    val colors = Waddle.colors
    val color = when (tone) {
        TagTone.NEUTRAL -> colors.fgMuted
        TagTone.GOLD -> colors.goldText
        TagTone.ACTION -> colors.actionText
        TagTone.SUCCESS -> colors.successText
        TagTone.LIVE -> colors.liveText
    }
    Box(
        modifier = modifier
            .height(TAG_HEIGHT)
            .border(HAIRLINE, colors.border, RoundedCornerShape(Waddle.radius.tag))
            .padding(horizontal = Waddle.space.xs),
        contentAlignment = Alignment.Center,
    ) {
        Text(text = text, style = Waddle.type.tag, color = color, maxLines = 1)
    }
}

private val BADGE_HEIGHT = 20.dp
private val BADGE_PADDING = 6.dp
private val TAG_HEIGHT = 22.dp
private val HAIRLINE = 1.dp
private const val MAX_COUNT = 99

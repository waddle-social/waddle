package social.waddle.android.ui.huddle

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.defaultMinSize
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
 * Huddle's kicker: mono 11, uppercase, tracked 0.08em, muted — the small
 * label above a section, a card or a count line ("FRI 16 OCT · 12 AROUND").
 * [live] turns it ember with a [LiveDot] in front, for what is happening
 * now. Kickers that head a section are headings for screen readers.
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
            text = text.uppercase(),
            style = Waddle.type.kicker,
            color = color,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

/** What a [CountBadge] counts, which decides its colour. */
enum class CountTone {
    /** Mentions and anything live: ember. */
    LIVE,

    /** Plain unread: neutral. */
    NEUTRAL,
}

/**
 * A small mono count (web `count` recipe): ember for mentions, neutral
 * for plain unread. Counts above 99 read "99+". [prefix] adds "@" for
 * mention counts.
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
        CountTone.LIVE -> colors.live to colors.liveFg
        CountTone.NEUTRAL -> colors.neutralBadge to colors.onNeutralBadge
    }
    Box(
        modifier = modifier
            .defaultMinSize(minWidth = BADGE_HEIGHT, minHeight = BADGE_HEIGHT)
            .background(fill, RoundedCornerShape(Waddle.radius.pill))
            .padding(horizontal = BADGE_PADDING),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            text = prefix + if (count > MAX_COUNT) "$MAX_COUNT+" else count.toString(),
            style = Waddle.type.time,
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
    MOSS,

    /** Live states: "Live", "Speaking". */
    LIVE,
}

/** A mono uppercase tag for roles and states: text only, no fill (web hat tags). */
@Composable
fun HuddleTag(text: String, modifier: Modifier = Modifier, tone: TagTone = TagTone.NEUTRAL) {
    val colors = Waddle.colors
    val color = when (tone) {
        TagTone.NEUTRAL -> colors.fgMuted
        TagTone.GOLD -> colors.goldText
        TagTone.ACTION -> colors.actionText
        TagTone.MOSS -> colors.mossText
        TagTone.LIVE -> colors.liveText
    }
    Text(
        text = text.uppercase(),
        style = Waddle.type.kicker,
        color = color,
        maxLines = 1,
        modifier = modifier,
    )
}

private val BADGE_HEIGHT = 20.dp
private val BADGE_PADDING = 6.dp
private const val MAX_COUNT = 99

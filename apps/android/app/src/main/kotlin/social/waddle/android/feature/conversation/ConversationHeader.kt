package social.waddle.android.feature.conversation

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import social.waddle.android.R
import social.waddle.android.client.store.PresenceShow
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.huddle.AvatarSize
import social.waddle.android.ui.huddle.HuddleAvatar
import social.waddle.android.ui.huddle.Kicker

/**
 * The conversation title block in the top bar: an optional leading face,
 * the name in Space Grotesk 16/600 (a heading), and a subline. One line
 * each, so a long name ellipsises instead of clipping the bar.
 */
@Composable
fun ConversationTitle(
    title: String,
    modifier: Modifier = Modifier,
    leading: (@Composable () -> Unit)? = null,
    subline: (@Composable () -> Unit)? = null,
) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
        modifier = modifier,
    ) {
        leading?.invoke()
        Column {
            Text(
                text = title,
                style = MaterialTheme.typography.titleMedium,
                color = Waddle.colors.fg,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.semantics { heading() },
            )
            subline?.invoke()
        }
    }
}

/** A live huddle in a room, as the header tells it. */
@Immutable
data class HeaderHuddle(val participants: Int, val video: Boolean)

/**
 * Under a room's name: who is here ("5 here", from the people
 * directory's headcount) and, when a huddle is live, the ember part:
 * "● Huddle · 3 in". Nothing when neither is known.
 */
@Composable
fun RoomHeaderSubline(here: Int?, huddle: HeaderHuddle?) {
    if (here == null && huddle == null) return
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs),
    ) {
        if (here != null) Kicker(text = pluralStringResource(R.plurals.room_header_here, here, here))
        if (huddle != null) {
            Kicker(
                text = pluralStringResource(
                    if (huddle.video) R.plurals.room_header_video_huddle else R.plurals.room_header_huddle,
                    huddle.participants,
                    huddle.participants,
                ),
                live = true,
            )
        }
    }
}

/** A DM peer's face with their presence dot, for [ConversationTitle]'s leading slot. */
@Composable
fun DmHeaderAvatar(jid: String, name: String, presence: PresenceShow) {
    HuddleAvatar(jid = jid, name = name, size = AvatarSize.MD, presence = presence)
}

/** "Available", "Away · idle 20m", "Do not disturb", "Offline": the word under a DM peer's name. */
@Composable
fun DmHeaderSubline(text: String) {
    Text(
        text = text,
        style = MetaStyle,
        color = Waddle.colors.fgMuted,
        maxLines = 1,
        overflow = TextOverflow.Ellipsis,
    )
}

/** The presence word for [show]; away shows its idle age when known. */
@Composable
fun presenceWordOf(show: PresenceShow, idleAge: String?): String = when (show) {
    PresenceShow.AVAILABLE -> stringResource(R.string.dm_presence_available)
    PresenceShow.AWAY, PresenceShow.XA -> idleAge?.let { stringResource(R.string.dm_presence_away_idle, it) }
        ?: stringResource(R.string.dm_presence_away)
    PresenceShow.DND -> stringResource(R.string.dm_presence_dnd)
    PresenceShow.OFFLINE -> stringResource(R.string.dm_presence_offline)
}

package social.waddle.android.ui.huddle

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import social.waddle.android.R
import social.waddle.android.avatar.PeerAvatar
import social.waddle.android.client.store.PresenceShow
import social.waddle.android.theme.Waddle

/**
 * A person, Huddle style: their avatar plus where they are.
 *
 * - [presence]: the dot at the bottom right — green here, amber away,
 *   red do not disturb, hollow offline. Offline also dims the whole
 *   avatar. `null` shows no dot (presence unknown).
 * - [inHuddle]: a teal ring, 2dp off the avatar.
 * - [speaking]: the ring turns ember and glows. Only honest for the call
 *   this device is in.
 *
 * [ground] is the colour the avatar sits on: the dot's border and the
 * ring's gap are cut from it.
 */
@Composable
fun HuddleAvatar(
    jid: String?,
    name: String,
    size: AvatarSize,
    modifier: Modifier = Modifier,
    presence: PresenceShow? = null,
    inHuddle: Boolean = false,
    speaking: Boolean = false,
    ground: Color = Waddle.colors.bg,
) {
    val colors = Waddle.colors
    val offline = presence == PresenceShow.OFFLINE
    val ringed = when {
        speaking -> Modifier.ring(gapColor = ground, ringColor = colors.live, glow = colors.glowLive)
        inHuddle -> Modifier.ring(gapColor = ground, ringColor = colors.action)
        else -> Modifier
    }
    val description = presenceDescription(presence, inHuddle, speaking)
    Box(
        modifier = modifier
            .size(size.dp)
            .then(if (description != null) Modifier.semantics { contentDescription = description } else Modifier),
    ) {
        PeerAvatar(
            jid = jid,
            displayName = name,
            size = size.dp,
            modifier = ringed.alpha(if (offline) OFFLINE_ALPHA else 1f),
        )
        if (presence != null) {
            PresenceDot(
                presence = presence,
                ground = ground,
                dotSize = if (size.dp <= SMALL_AVATAR) SMALL_DOT else DOT,
                modifier = Modifier
                    .align(Alignment.BottomEnd)
                    .offset(x = DOT_OUTSET, y = DOT_OUTSET),
            )
        }
    }
}

/** The presence dot on its own, for rows that show presence without an avatar. */
@Composable
fun PresenceDot(
    presence: PresenceShow,
    modifier: Modifier = Modifier,
    ground: Color = Waddle.colors.bg,
    dotSize: Dp = DOT,
) {
    val colors = Waddle.colors
    val fill = when (presence) {
        PresenceShow.AVAILABLE -> colors.presenceAvailable
        PresenceShow.AWAY, PresenceShow.XA -> colors.presenceAway
        PresenceShow.DND -> colors.presenceDnd
        PresenceShow.OFFLINE -> ground
    }
    Box(
        modifier = modifier
            .size(dotSize)
            .background(fill, CircleShape)
            .border(DOT_BORDER, ground, CircleShape),
        contentAlignment = Alignment.Center,
    ) {
        if (presence == PresenceShow.OFFLINE) {
            // Hollow: the ground shows through a thin offline ring.
            Box(
                modifier = Modifier
                    .size(dotSize - DOT_BORDER * 2)
                    .border(HOLLOW_STROKE, colors.presenceOffline, CircleShape),
            )
        }
    }
}

/** What the ring and dot say, for screen readers. */
@Composable
private fun presenceDescription(presence: PresenceShow?, inHuddle: Boolean, speaking: Boolean): String? {
    val where = when {
        speaking -> stringResource(R.string.presence_speaking)
        inHuddle -> stringResource(R.string.presence_in_huddle)
        else -> null
    }
    val show = when (presence) {
        PresenceShow.AVAILABLE -> stringResource(R.string.presence_available)
        PresenceShow.AWAY, PresenceShow.XA -> stringResource(R.string.presence_away)
        PresenceShow.DND -> stringResource(R.string.presence_dnd)
        PresenceShow.OFFLINE -> stringResource(R.string.presence_offline)
        null -> null
    }
    return listOfNotNull(show, where).takeIf { it.isNotEmpty() }?.joinToString(", ")
}

private const val OFFLINE_ALPHA = 0.55f
private val SMALL_AVATAR = 28.dp
private val DOT = 12.dp
private val SMALL_DOT = 10.dp
private val DOT_BORDER = 2.dp
private val HOLLOW_STROKE = 1.5.dp
private val DOT_OUTSET = 1.dp

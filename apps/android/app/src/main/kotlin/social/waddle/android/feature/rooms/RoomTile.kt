package social.waddle.android.feature.rooms

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Call
import androidx.compose.material.icons.outlined.Forum
import androidx.compose.material.icons.outlined.Tag
import androidx.compose.material.icons.outlined.Videocam
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import social.waddle.android.R
import social.waddle.android.client.normalizedBareJid
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.huddle.AvatarSize
import social.waddle.android.ui.huddle.CountBadge
import social.waddle.android.ui.huddle.CountTone
import social.waddle.android.ui.huddle.HuddleAvatar
import social.waddle.android.ui.huddle.HuddleCard
import social.waddle.android.ui.huddle.Kicker
import social.waddle.android.ui.huddle.PillButton
import social.waddle.android.ui.huddle.PillTone

/** A room tile's kicker as words (see [roomKickerOf]). */
@Composable
fun roomKickerText(kicker: RoomKicker): String = when (kicker) {
    is RoomKicker.Huddle -> stringResource(
        if (kicker.video) R.string.room_kicker_video_huddle else R.string.room_kicker_voice_huddle,
        kicker.count,
    )
    is RoomKicker.Here -> pluralStringResource(R.plurals.room_kicker_here, kicker.count, kicker.count)
    is RoomKicker.Mentions -> pluralStringResource(R.plurals.room_kicker_mentions, kicker.count, kicker.count)
    is RoomKicker.Unread -> pluralStringResource(R.plurals.room_kicker_unread, kicker.count, kicker.count)
    RoomKicker.Active -> stringResource(R.string.room_kicker_active)
    RoomKicker.Quiet -> stringResource(R.string.room_kicker_quiet)
    RoomKicker.Room -> stringResource(R.string.room_kicker_room)
}

/**
 * One room on the Rooms tab (web `room-card`): what is happening as a
 * kicker, the name, the last message (or an invitation when it is
 * quiet), who is in the huddle, and one way in. A live huddle turns the
 * tile ember and glows; nothing else does.
 */
@Composable
fun RoomTile(
    room: RoomActivity,
    nowMillis: Long,
    onEnter: () -> Unit,
    onJoin: () -> Unit,
    modifier: Modifier = Modifier,
    speaking: Set<String> = emptySet(),
) {
    val colors = Waddle.colors
    HuddleCard(modifier = modifier.fillMaxWidth(), live = room.live, onClick = onEnter) {
        Kicker(
            text = roomKickerText(roomKickerOf(room, nowMillis)),
            live = room.live,
            color = if (room.live) colors.liveText else colors.fgMuted,
        )
        Spacer(Modifier.size(Waddle.space.xs))
        RoomTitle(name = room.name, forum = room.forum)
        roomBodyOf(room)?.let { body ->
            Spacer(Modifier.size(Waddle.space.xxs))
            Text(
                text = body,
                style = MaterialTheme.typography.bodyMedium,
                color = colors.fgMuted,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
        }
        Spacer(Modifier.size(Waddle.space.sm))
        Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth()) {
            if (room.huddle.isNotEmpty()) {
                HuddleStack(participants = room.huddle, speaking = speaking)
                Spacer(Modifier.width(Waddle.space.xs))
            }
            RoomBadge(room)
            Spacer(Modifier.weight(1f))
            RoomPill(room = room, onEnter = onEnter, onJoin = onJoin)
        }
    }
}

/** `#` (or the forum glyph) in teal, then the room's name in the display style. */
@Composable
fun RoomTitle(name: String, forum: Boolean, modifier: Modifier = Modifier) {
    Row(
        modifier = modifier,
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.xxs),
    ) {
        Icon(
            if (forum) Icons.Outlined.Forum else Icons.Outlined.Tag,
            contentDescription = null,
            tint = Waddle.colors.actionText,
            modifier = Modifier.size(TITLE_GLYPH),
        )
        Text(
            text = name,
            style = Waddle.type.title,
            color = Waddle.colors.fg,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

/**
 * The tile's body: the newest message ("mara: shipped it"), else for a
 * joined, quiet room the invitation, else the room's own description.
 * A room we are not in is never called quiet.
 */
@Composable
fun roomBodyOf(room: RoomActivity): AnnotatedString? {
    val preview = room.preview
    return when {
        preview != null -> buildAnnotatedString {
            room.previewAuthor?.let { author ->
                withStyle(SpanStyle(color = Waddle.colors.fg, fontWeight = FontWeight.SemiBold)) { append(author) }
                append(": ")
            }
            append(preview)
        }
        room.joined && !room.live && room.loudUnread == 0 -> AnnotatedString(stringResource(R.string.room_quiet_line))
        room.description != null -> AnnotatedString(room.description)
        else -> null
    }
}

/** Mentions in ember with an @, else plain unread in neutral. */
@Composable
private fun RoomBadge(room: RoomActivity) {
    when {
        room.mentions > 0 -> {
            val description = pluralStringResource(R.plurals.room_mentions_description, room.mentions, room.mentions)
            CountBadge(
                count = room.mentions,
                tone = CountTone.LIVE,
                prefix = "@",
                modifier = Modifier.clearAndSetSemantics { contentDescription = description },
            )
        }
        room.loudUnread > 0 -> {
            val description = pluralStringResource(R.plurals.room_unread_description, room.loudUnread, room.loudUnread)
            CountBadge(
                count = room.loudUnread,
                modifier = Modifier.clearAndSetSemantics { contentDescription = description },
            )
        }
    }
}

/** Ember "Join" while there is a huddle to join, else a quiet "Enter". */
@Composable
private fun RoomPill(room: RoomActivity, onEnter: () -> Unit, onJoin: () -> Unit) {
    if (room.canJoin) {
        val description = stringResource(R.string.room_join_description, room.name)
        PillButton(
            text = stringResource(R.string.room_join),
            onClick = onJoin,
            tone = PillTone.LIVE,
            icon = if (room.video) Icons.Outlined.Videocam else Icons.Outlined.Call,
            modifier = Modifier.semantics { contentDescription = description },
        )
    } else {
        val description = stringResource(R.string.room_enter_description, room.name)
        PillButton(
            text = stringResource(R.string.room_enter),
            onClick = onEnter,
            tone = PillTone.QUIET,
            modifier = Modifier.semantics { contentDescription = description },
        )
    }
}

/**
 * Up to [max] huddle avatars overlapping by 6dp, each cut out of the
 * card by a 2dp ring (web `room-card__avatars`). Speakers glow.
 */
@Composable
fun HuddleStack(
    participants: List<HuddleParticipant>,
    modifier: Modifier = Modifier,
    speaking: Set<String> = emptySet(),
    max: Int = MAX_STACK,
    ground: Color = Waddle.colors.surface,
) {
    val speakers = speaking.mapTo(HashSet(), ::normalizedBareJid)
    val shown = participants.take(max)
    // Negative spacing overlaps the circles; later ones draw on top.
    Row(
        modifier = modifier.clearAndSetSemantics { },
        horizontalArrangement = Arrangement.spacedBy(-STACK_OVERLAP),
    ) {
        shown.forEach { person ->
            Box(
                modifier = Modifier
                    .background(ground, CircleShape)
                    .padding(STACK_RING),
            ) {
                HuddleAvatar(
                    jid = person.jid,
                    name = person.nick,
                    size = AvatarSize.XS,
                    speaking = person.jid?.let(::normalizedBareJid) in speakers,
                    ground = ground,
                )
            }
        }
    }
}

private const val MAX_STACK = 4
private val STACK_OVERLAP = 6.dp
private val STACK_RING = 2.dp
private val TITLE_GLYPH = 18.dp

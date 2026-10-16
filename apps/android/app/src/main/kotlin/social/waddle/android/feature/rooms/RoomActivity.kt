package social.waddle.android.feature.rooms

import social.waddle.android.client.calls.CallState
import social.waddle.android.client.calls.MucCallMedia
import social.waddle.android.client.store.InboxEntrySnapshot
import social.waddle.android.client.store.NotifySettingsEntry
import social.waddle.android.client.store.RoomHeadcount
import social.waddle.android.feature.call.localCallInRoom
import social.waddle.android.feature.call.normalizeCallRoomJid
import social.waddle.android.feature.call.resolveRoomParticipantList
import social.waddle.android.jid.bareJidOf
import social.waddle.android.jid.localpartOf
import social.waddle.client.ffi.WaddleChannel
import social.waddle.client.ffi.WaddleNotifyMode

/** Someone in a room's huddle: their room nick and, when the room shows it, their real bare JID. */
data class HuddleParticipant(val nick: String, val jid: String?)

/**
 * One room as the shell shows it: what is happening there right now.
 * Every field is something Android knows; nothing is estimated.
 */
data class RoomActivity(
    val roomJid: String,
    val name: String,
    val description: String? = null,
    val forum: Boolean = false,
    /** Whether this account is in the room, so [here] is real. */
    val joined: Boolean = false,
    /** Others in the room right now; `null` when not joined (unknown). */
    val here: Int? = null,
    /** The live huddle's participants, in join order; empty when none. */
    val huddle: List<HuddleParticipant> = emptyList(),
    val video: Boolean = false,
    /** This device is in this room's huddle. */
    val youAreInHuddle: Boolean = false,
    /** The call slot is busy with a call somewhere else. */
    val busyElsewhere: Boolean = false,
    /** Unread mentions (live-only, see `MentionStore`). */
    val mentions: Int = 0,
    val unread: Int = 0,
    val threadUnread: Int = 0,
    /** XEP-0492 `never`: unread stays quiet. */
    val muted: Boolean = false,
    /** The newest message, from the server inbox. */
    val preview: String? = null,
    val previewAuthor: String? = null,
    val lastUpdated: Long? = null,
) {
    val live: Boolean get() = huddle.isNotEmpty()

    /** Unread that asks for attention: a muted room's unread does not. */
    val loudUnread: Int get() = if (muted) 0 else unread

    /** A live huddle this device can join from here. */
    val canJoin: Boolean get() = live && !youAreInHuddle && !busyElsewhere

    /** Someone expects this account: a mention or a thread reply waiting (web "needs someone like you"). */
    val needsYou: Boolean get() = mentions > 0 || threadUnread > 0

    /** Anything at all to catch up on. */
    val hasActivity: Boolean get() = mentions > 0 || loudUnread > 0 || threadUnread > 0
}

/** Everything the room projection reads, as of one instant. */
data class RoomInputs(
    val channels: List<WaddleChannel>,
    val joinedRooms: Set<String>,
    /** People directory headcounts (normalised room JID keys, any-case lookups). */
    val headcounts: Map<String, RoomHeadcount>,
    val mentions: Map<String, Int>,
    val unread: Map<String, Int>,
    val threadUnread: Map<String, Int>,
    val inbox: Map<String, InboxEntrySnapshot>,
    val notify: Map<String, NotifySettingsEntry>,
    val calls: RoomCallInputs,
)

/** The XEP-0272 and LiveKit views that decide who is in each room's huddle. */
data class RoomCallInputs(
    val participants: Map<String, Set<String>> = emptyMap(),
    val owners: Map<String, Map<String, String?>> = emptyMap(),
    val media: Map<String, MucCallMedia> = emptyMap(),
    val live: Map<String, List<String>> = emptyMap(),
    val leaving: Map<String, String> = emptyMap(),
    val state: CallState = CallState.Idle,
)

/** The listable rooms (no group DMs), in topology order. */
fun roomActivitiesOf(inputs: RoomInputs): List<RoomActivity> {
    val joined = inputs.joinedRooms.mapTo(HashSet()) { normalizeCallRoomJid(it) }
    val slotBusy = inputs.calls.state !is CallState.Idle && inputs.calls.state !is CallState.Ended
    return inputs.channels.filterNot { it.isGroupDm }.map { channel ->
        val bare = bareJidOf(channel.roomJid)
        val room = normalizeCallRoomJid(channel.roomJid)
        val isJoined = room in joined
        val nicks = resolveRoomParticipantList(
            room,
            inputs.calls.participants,
            inputs.calls.owners,
            inputs.calls.live,
            inputs.calls.leaving,
        )
        val owners = inputs.calls.owners[room].orEmpty()
        val inHere = localCallInRoom(inputs.calls.state, room)
        val entry = inputs.inbox[bare]
        RoomActivity(
            roomJid = channel.roomJid,
            name = channel.bookmarkName?.takeIf { it.isNotBlank() } ?: channel.name,
            description = channel.description?.trim()?.takeIf { it.isNotEmpty() },
            forum = channel.channelType == FORUM_TYPE,
            joined = isJoined,
            here = if (isJoined) inputs.headcounts[room]?.here ?: 0 else null,
            huddle = nicks.map { nick -> HuddleParticipant(nick, owners[nick]?.let(::bareJidOf)) },
            video = inputs.calls.media[room]?.video == true,
            youAreInHuddle = inHere,
            busyElsewhere = slotBusy && !inHere,
            mentions = inputs.mentions[bare] ?: 0,
            unread = inputs.unread[bare] ?: 0,
            threadUnread = inputs.threadUnread[bare] ?: 0,
            muted = inputs.notify[bare]?.notifyMode == WaddleNotifyMode.NEVER,
            preview = entry?.preview?.trim()?.takeIf { it.isNotEmpty() },
            previewAuthor = entry?.author?.let(::authorOf),
            lastUpdated = entry?.lastUpdated,
        )
    }
}

/** A room inbox author is the occupant JID (`room@muc/nick`); a bare JID names a person. */
private fun authorOf(author: String): String? {
    val nick = author.substringAfter('/', missingDelimiterValue = "")
    return nick.ifEmpty { localpartOf(author) }.takeIf { it.isNotBlank() }
}

/**
 * The Rooms sort (web `RoomsPage`): the biggest huddle first, then
 * mentions, unread, the newest message, then name and JID so equal rooms
 * never swap places between two sorts.
 */
val RoomActivityOrder: Comparator<RoomActivity> =
    compareByDescending<RoomActivity> { it.huddle.size }
        .thenByDescending { it.mentions }
        .thenByDescending { it.loudUnread }
        .thenByDescending { it.lastUpdated ?: Long.MIN_VALUE }
        .thenBy(String.CASE_INSENSITIVE_ORDER) { it.name }
        .thenBy { it.roomJid }

/** What a room tile's kicker says: the first true thing in [roomKickerOf]'s order. */
sealed interface RoomKicker {
    data class Huddle(val video: Boolean, val count: Int) : RoomKicker

    data class Here(val count: Int) : RoomKicker

    data class Mentions(val count: Int) : RoomKicker

    data class Unread(val count: Int) : RoomKicker

    /** A message landed within [ACTIVE_WINDOW_MILLIS]. */
    data object Active : RoomKicker

    /** Joined, nobody else here, nothing new: an invitation, not a zero. */
    data object Quiet : RoomKicker

    /** Not joined: Android does not know who is there, so it claims nothing. */
    data object Room : RoomKicker
}

/**
 * Kicker rules (web `RoomsPage`, Android edition): a live huddle, then
 * how many are here (real for every joined room on Android), mentions,
 * unread, recent activity, else quiet. A room this account is not in is
 * just "Room": never "quiet", because nobody knows.
 */
fun roomKickerOf(room: RoomActivity, nowMillis: Long): RoomKicker {
    val recent = room.lastUpdated?.let { nowMillis - it in 0..ACTIVE_WINDOW_MILLIS } == true
    return when {
        room.live -> RoomKicker.Huddle(video = room.video, count = room.huddle.size)
        room.here != null && room.here > 0 -> RoomKicker.Here(room.here)
        room.mentions > 0 -> RoomKicker.Mentions(room.mentions)
        room.loudUnread > 0 -> RoomKicker.Unread(room.loudUnread)
        recent -> RoomKicker.Active
        room.joined -> RoomKicker.Quiet
        else -> RoomKicker.Room
    }
}

/** "Room · active" means a message in the last quarter hour. */
const val ACTIVE_WINDOW_MILLIS = 15 * 60 * 1000L

private const val FORUM_TYPE = "forum"

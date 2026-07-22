package social.waddle.android.client.store

import kotlinx.coroutines.flow.first
import social.waddle.android.client.prefs.SessionPrefs
import java.time.Instant
import java.time.OffsetDateTime

/**
 * The in-memory domain stores of one signed-in session: created once
 * with the session manager, wiped via [clear] on login/logout, and
 * re-seeded from persistence via [seedFromPrefs].
 */
internal class SessionStores {
    val timelineStore = TimelineStore()
    val roomStore = RoomStore()
    val presenceStore = PresenceStore()
    val dmStore = DmStore()
    val unreadStore = UnreadStore()
    val inboxStore = InboxStore()
    val chatStateStore = ChatStateStore()
    val readCursorStore = ReadCursorStore()
    val pinStore = PinStore()
    val notifySettingsStore = NotifySettingsStore()
    val roomMembersStore = RoomMembersStore()
    val stickerPackStore = StickerPackStore()
    val profileStore = ProfileStore()

    fun clear() {
        timelineStore.clear()
        timelineStore.setOwnBareJid(null)
        roomStore.clear()
        presenceStore.clear()
        dmStore.clear()
        unreadStore.clearAll()
        inboxStore.clear()
        chatStateStore.clear()
        readCursorStore.clear()
        pinStore.clear()
        notifySettingsStore.clear()
        roomMembersStore.clear()
        stickerPackStore.clear()
        profileStore.clear()
    }

    /** Restore joined rooms and the DM recency list from [sessionPrefs]. */
    suspend fun seedFromPrefs(sessionPrefs: SessionPrefs) {
        val joinedRooms = sessionPrefs.joinedRooms.first()
        roomStore.replaceJoinedRooms(joinedRooms)
        dmStore.seed(
            sessionPrefs.lastSeen.first()
                .filterKeys { it !in joinedRooms }
                .entries
                // Parsed comparison: the markers mix server offsets
                // (+00:00) with local ones (+02:00), and a raw string
                // sort would misorder them across timezones.
                .sortedBy { entry -> parseInstantOrNull(entry.value) }
                .map { it.key },
        )
    }

    private fun parseInstantOrNull(value: String): Instant? =
        runCatching { Instant.parse(value) }.getOrNull()
            ?: runCatching { OffsetDateTime.parse(value).toInstant() }.getOrNull()
}

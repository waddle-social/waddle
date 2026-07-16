package social.waddle.android.client.store

import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.update
import social.waddle.android.client.bareJid
import social.waddle.client.ffi.WaddleBookmarkItem
import social.waddle.client.ffi.WaddleDmBookmarkItem
import social.waddle.client.ffi.WaddleNotifyMode
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong

/**
 * XEP-0492 §3 conversation kinds for default-mode resolution. The
 * client cannot yet discriminate public from private rooms (the
 * topology carries no `members-only` flag), so callers pass
 * [PRIVATE_GROUP] for every MUC — web parity (`notify-settings.ts`).
 */
enum class ConversationKind {
    DIRECT_CHAT,
    PRIVATE_GROUP,
    PUBLIC_GROUP,
}

/**
 * XEP-0492 §3 default in the absence of any notification settings:
 * "always" for direct chats and private group chats, "on-mention" for
 * public group chats.
 */
fun defaultNotifyMode(kind: ConversationKind): WaddleNotifyMode = when (kind) {
    ConversationKind.DIRECT_CHAT, ConversationKind.PRIVATE_GROUP -> WaddleNotifyMode.ALWAYS
    ConversationKind.PUBLIC_GROUP -> WaddleNotifyMode.ON_MENTION
}

/**
 * One conversation's stored XEP-0492 state. A `null` [notifyMode]
 * means the bookmark exists but carries no fallback setting — the
 * effective mode still resolves to the §3 default.
 */
data class NotifySettingsEntry(
    val notifyMode: WaddleNotifyMode?,
    val richPayloadOptIn: Boolean,
)

/**
 * XEP-0492 notification settings per conversation, hydrated from the
 * XEP-0402 bookmarks (rooms) + `urn:waddle:dm-bookmarks:0` (DMs) on
 * every fresh session-ready and reconciled from the set-verb outcomes.
 *
 * No PEP `+notify` subscription yet (web parity): a change made on
 * another device reaches this one on the next fresh-stream hydrate.
 */
class NotifySettingsStore {
    private val _entries = MutableStateFlow<Map<String, NotifySettingsEntry>>(emptyMap())

    /** conversation bare JID → stored XEP-0492 entry. */
    val entries: StateFlow<Map<String, NotifySettingsEntry>> = _entries.asStateFlow()

    /**
     * Bumped by [clear] and every set-verb reconcile; an in-flight
     * [hydrate] whose fetches straddled the bump discards its commit —
     * a snapshot raced by a local write (or a logout) is stale.
     */
    private val generation = AtomicLong(0)

    /** Re-entrancy guard: one hydrate at a time (web parity). */
    private val hydrating = AtomicBoolean(false)

    /** The effective XEP-0492 mode: stored fallback or the §3 default. */
    fun modeFor(jid: String, kind: ConversationKind): WaddleNotifyMode =
        _entries.value[bareJid(jid)]?.notifyMode ?: defaultNotifyMode(kind)

    /** [modeFor] as a cold flow for UI state combines. */
    fun modeFlow(jid: String, kind: ConversationKind): Flow<WaddleNotifyMode> {
        val key = bareJid(jid)
        return _entries.map { it[key]?.notifyMode ?: defaultNotifyMode(kind) }
    }

    /** The Waddle rich XEP-0357 push-summary opt-in (#719); off by default. */
    fun richPayloadOptIn(jid: String): Boolean =
        _entries.value[bareJid(jid)]?.richPayloadOptIn ?: false

    /**
     * Fetch both carriers and replace the whole map. Commit-on-success
     * only — the previous entries stay visible while the fetches run
     * (no clear-before-fetch flicker, web parity), and a commit is
     * discarded when [clear] or a set-verb reconcile landed mid-flight
     * (the local write is newer than this snapshot).
     */
    suspend fun hydrate(
        fetchRoomBookmarks: suspend () -> List<WaddleBookmarkItem>,
        fetchDmBookmarks: suspend () -> List<WaddleDmBookmarkItem>,
    ) {
        if (!hydrating.compareAndSet(false, true)) return
        try {
            val startGeneration = generation.get()
            val fetched = buildMap {
                fetchRoomBookmarks().forEach { item ->
                    put(bareJid(item.jid), NotifySettingsEntry(item.notifyMode, item.richPayloadOptIn))
                }
                fetchDmBookmarks().forEach { item ->
                    put(bareJid(item.jid), NotifySettingsEntry(item.notifyMode, item.richPayloadOptIn))
                }
            }
            if (generation.get() == startGeneration) {
                _entries.value = fetched
            }
        } finally {
            hydrating.set(false)
        }
    }

    /** Reconcile a successful room set-verb outcome (no refetch). */
    fun applyRoomUpdate(item: WaddleBookmarkItem) {
        applyEntry(item.jid, NotifySettingsEntry(item.notifyMode, item.richPayloadOptIn))
    }

    /** Reconcile a successful DM set-verb `Ok` outcome (no refetch). */
    fun applyDmUpdate(item: WaddleDmBookmarkItem) {
        applyEntry(item.jid, NotifySettingsEntry(item.notifyMode, item.richPayloadOptIn))
    }

    /**
     * Reconcile a DM `Removed` outcome: the sparse DM node retracted
     * the item, so the entry drops and the effective mode falls back
     * to the §3 direct-chat default.
     */
    fun applyDmRemoved(jid: String) {
        generation.incrementAndGet()
        _entries.update { it - bareJid(jid) }
    }

    fun clear() {
        generation.incrementAndGet()
        _entries.value = emptyMap()
    }

    private fun applyEntry(jid: String, entry: NotifySettingsEntry) {
        generation.incrementAndGet()
        _entries.update { it + (bareJid(jid) to entry) }
    }
}

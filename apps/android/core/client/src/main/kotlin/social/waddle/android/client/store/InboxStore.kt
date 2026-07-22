package social.waddle.android.client.store

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import social.waddle.android.client.bareJid
import social.waddle.client.ffi.WaddleInboxEntry

/** Conversation surface of one XEP-0430 inbox entry. */
enum class InboxKind { DIRECT, MUC }

/** Reconciled snapshot of one inbox conversation (or room thread). */
data class InboxEntrySnapshot(
    val partner: String,
    val kind: InboxKind,
    val lastStanzaId: String?,
    val lastUpdated: Long?,
    val unread: Int,
    val preview: String?,
    val threadId: String?,
    val threadTitle: String?,
    val replyCount: Int?,
    val author: String?,
)

/** A room thread's inbox identity: room bare JID + thread id. */
data class InboxThreadKey(val roomJid: String, val threadId: String)

/**
 * Server-authoritative XEP-0430 inbox state, reconciled with the web
 * client's rules: hydrate pages and live pushes absolute-SET each
 * conversation's snapshot, guarded by
 *
 *  1. a freshness check — an entry older than the stored one
 *     (`last-updated`, tie-broken on `last_stanza_id`/unread) is
 *     dropped so a late hydrate can never regress a newer push;
 *  2. a read-clear barrier — a local read snapshots the entry's
 *     `last_stanza_id`, and a racing push that still carries that id
 *     is clamped to zero unread (only a genuinely NEWER message voids
 *     the barrier); web `rememberReadClear` parity.
 *
 * Direct and MUC conversations live in separate maps; room-thread
 * entries are keyed `roomJid+threadId` (no thread UI yet). The store
 * also remembers which newest-message stanza ids the server inbox has
 * already accounted, so the live-message unread increment can dedupe
 * against a push that arrived first (web `wasUnreadAccountedByInbox`).
 */
class InboxStore {
    private val lock = Any()

    private val _direct = MutableStateFlow<Map<String, InboxEntrySnapshot>>(emptyMap())

    /** Direct conversations: peer bare JID → reconciled snapshot. */
    val direct: StateFlow<Map<String, InboxEntrySnapshot>> = _direct.asStateFlow()

    private val _muc = MutableStateFlow<Map<String, InboxEntrySnapshot>>(emptyMap())

    /** Room conversations: room bare JID → reconciled snapshot. */
    val muc: StateFlow<Map<String, InboxEntrySnapshot>> = _muc.asStateFlow()

    private val _threads = MutableStateFlow<Map<InboxThreadKey, InboxEntrySnapshot>>(emptyMap())

    /** Room-thread entries, keyed room bare JID + thread id. */
    val threads: StateFlow<Map<InboxThreadKey, InboxEntrySnapshot>> = _threads.asStateFlow()

    /** Read-clear barriers: conversation(+thread) → snapshotted stanza id. */
    private val readClearBarriers = HashMap<BarrierKey, String>()

    /** Per-conversation ring of server-accounted newest stanza ids. */
    private val accountedMessageIds = HashMap<String, LinkedHashSet<String>>()

    /**
     * Absolute-set one hydrate/push entry, subject to the freshness
     * guard and the read-clear barrier. Returns the stored snapshot,
     * or `null` when the entry was dropped as stale.
     */
    fun applyEntry(entry: WaddleInboxEntry): InboxEntrySnapshot? = synchronized(lock) {
        val incoming = snapshotOf(entry)
        val existing = existingFor(incoming)
        if (isStale(incoming, existing)) return null
        val reconciled = withReadClearBarrier(incoming)
        store(reconciled)
        rememberAccounted(reconciled)
        reconciled
    }

    /**
     * Local read: zero the stored unread and arm the read-clear
     * barrier at the entry's current `last_stanza_id`, so a racing
     * server push still carrying that id cannot resurrect the badge.
     * A conversation the inbox never reported has nothing to barrier.
     */
    fun markReadLocally(conversationJid: String, threadId: String? = null) {
        synchronized(lock) {
            val conversation = bareJid(conversationJid)
            val existing = lookup(conversation, threadId) ?: return
            existing.lastStanzaId?.let {
                readClearBarriers[BarrierKey(conversation, threadId)] = it
            }
            if (existing.unread != 0) store(existing.copy(unread = 0))
        }
    }

    /**
     * Whether the server inbox already accounted one of [messageIds]
     * as a conversation's newest message — the live unread increment
     * skips such messages (web `wasUnreadAccountedByInbox` parity).
     */
    fun wasUnreadAccounted(conversationJid: String, messageIds: Collection<String>): Boolean =
        synchronized(lock) {
            val ids = accountedMessageIds[bareJid(conversationJid)] ?: return false
            messageIds.any { it in ids }
        }

    fun clear() {
        synchronized(lock) {
            _direct.value = emptyMap()
            _muc.value = emptyMap()
            _threads.value = emptyMap()
            readClearBarriers.clear()
            accountedMessageIds.clear()
        }
    }

    // ── Reconciliation internals (all under [lock]) ─────────────────────

    private fun snapshotOf(entry: WaddleInboxEntry) = InboxEntrySnapshot(
        partner = bareJid(entry.partner),
        kind = if (entry.kind == "muc") InboxKind.MUC else InboxKind.DIRECT,
        lastStanzaId = entry.lastStanzaId,
        lastUpdated = entry.lastUpdated,
        unread = entry.unread.toInt(),
        preview = entry.preview,
        threadId = entry.threadId,
        threadTitle = entry.threadTitle,
        replyCount = entry.replyCount?.toInt(),
        author = entry.author,
    )

    private fun existingFor(snapshot: InboxEntrySnapshot): InboxEntrySnapshot? {
        val threadId = snapshot.threadId
        return if (threadId != null) {
            _threads.value[InboxThreadKey(snapshot.partner, threadId)]
        } else {
            when (snapshot.kind) {
                InboxKind.DIRECT -> _direct.value[snapshot.partner]
                InboxKind.MUC -> _muc.value[snapshot.partner]
            }
        }
    }

    /** Kind-agnostic lookup for the local read path. */
    private fun lookup(conversation: String, threadId: String?): InboxEntrySnapshot? =
        if (threadId != null) {
            _threads.value[InboxThreadKey(conversation, threadId)]
        } else {
            _direct.value[conversation] ?: _muc.value[conversation]
        }

    /**
     * Web `applyFreshEntry` parity: drop entries older by
     * `last-updated`, and on a tie drop an entry that names a
     * DIFFERENT newest stanza without carrying more unread (a
     * reordered stale duplicate, not new information).
     */
    private fun isStale(incoming: InboxEntrySnapshot, existing: InboxEntrySnapshot?): Boolean {
        if (existing == null) return false
        val incomingUpdated = incoming.lastUpdated ?: Long.MIN_VALUE
        val existingUpdated = existing.lastUpdated ?: Long.MIN_VALUE
        if (incomingUpdated < existingUpdated) return true
        return incomingUpdated == existingUpdated &&
            incoming.lastStanzaId != existing.lastStanzaId &&
            incoming.unread <= existing.unread
    }

    /**
     * Web `applyReadClearBarrier` parity: an armed barrier clamps an
     * entry still naming the read stanza to zero unread; an entry
     * with a NEWER stanza voids the barrier and applies untouched.
     */
    private fun withReadClearBarrier(incoming: InboxEntrySnapshot): InboxEntrySnapshot {
        val key = BarrierKey(incoming.partner, incoming.threadId)
        val barrier = readClearBarriers[key] ?: return incoming
        return if (incoming.lastStanzaId == barrier) {
            if (incoming.unread == 0) incoming else incoming.copy(unread = 0)
        } else {
            readClearBarriers.remove(key)
            incoming
        }
    }

    private fun store(snapshot: InboxEntrySnapshot) {
        val threadId = snapshot.threadId
        if (threadId != null) {
            _threads.value =
                _threads.value + (InboxThreadKey(snapshot.partner, threadId) to snapshot)
        } else {
            when (snapshot.kind) {
                InboxKind.DIRECT -> _direct.value = _direct.value + (snapshot.partner to snapshot)
                InboxKind.MUC -> _muc.value = _muc.value + (snapshot.partner to snapshot)
            }
        }
    }

    private fun rememberAccounted(snapshot: InboxEntrySnapshot) {
        val id = snapshot.lastStanzaId ?: return
        val ids = accountedMessageIds.getOrPut(snapshot.partner) { LinkedHashSet() }
        ids.add(id)
        while (ids.size > ACCOUNTED_MESSAGE_ID_CAP) {
            ids.remove(ids.first())
        }
    }

    private data class BarrierKey(val conversation: String, val threadId: String?)

    private companion object {
        /** Web parity: remember at most 20 accounted ids per peer. */
        const val ACCOUNTED_MESSAGE_ID_CAP = 20
    }
}

package social.waddle.android.service

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.jid.bareJidOf
import social.waddle.client.ffi.WaddleMessage

/**
 * Which messages already raised a notification, by XEP-0359 stanza-id,
 * so the live path and the push path never announce the same message
 * twice, whichever arrives first.
 *
 * A push names the archived message's stanza-id ([PushSummary.item]); a
 * live message carries one per archiving entity. The XEP-0160 offline
 * flush replays a message that push announced while the device was
 * offline, so it must be recognised by id. Its XEP-0203 `<delay/>` alone
 * proves nothing: the server also stamps one on XEP-0198 redeliveries to
 * a sibling resource, which push never announced.
 *
 * The ids push posted are persisted ([PushedItemsStore]): the process
 * that posted a push may die before the flush arrives in a new one.
 * Both claims are atomic check-and-record operations, so a push and the
 * live copy of the same message racing each other produce one banner.
 */
class AnnouncedItems(
    private val store: PushedItemsStore,
    private val capacity: Int = DEFAULT_CAPACITY,
) {
    private val mutex = Mutex()
    private val liveLock = Any()
    private val live = LinkedHashSet<String>()
    private val pushed = LinkedHashSet<String>()
    private var loaded = false

    /**
     * The live path takes [message]: `false` when push already announced
     * it, otherwise its trusted ids are recorded as handled live.
     * [ownBareJid] is the account, the authority of a 1:1 message's id.
     */
    suspend fun claimLive(message: WaddleMessage, ownBareJid: String?): Boolean {
        val ids = trustedIdsOf(message, ownBareJid)
        return mutex.withLock {
            loadOnce()
            if (ids.any { it in pushed }) return@withLock false
            synchronized(liveLock) { remember(live, ids) }
            true
        }
    }

    /**
     * The push path takes [item]: `false` when the live path already
     * handled it, otherwise it is recorded (and persisted) as announced
     * by push before the caller posts.
     */
    suspend fun claimPush(item: String): Boolean = mutex.withLock {
        loadOnce()
        if (handledLive(item) || item in pushed) return@withLock false
        remember(pushed, listOf(item))
        saveQuietly(pushed.toList())
        true
    }

    /** Whether the live path already handled the message with stanza-id [item]. */
    fun handledLive(item: String): Boolean = synchronized(liveLock) { item in live }

    /** Sign-out: forget everything, on disk too. */
    suspend fun clear() = mutex.withLock {
        synchronized(liveLock) { live.clear() }
        pushed.clear()
        loaded = true
        saveQuietly(emptyList())
    }

    private suspend fun loadOnce() {
        if (loaded) return
        // An unreadable store degrades to this process's memory only.
        remember(pushed, quietly { store.load() }.orEmpty())
        loaded = true
    }

    /** A failed write keeps the in-memory dedupe; it must not drop the post. */
    private suspend fun saveQuietly(items: List<String>) {
        quietly { store.save(items) }
    }

    private suspend fun <T> quietly(action: suspend () -> T): T? = try {
        action()
    } catch (cancellation: CancellationException) {
        throw cancellation
    } catch (_: Exception) {
        null
    }

    private fun remember(into: LinkedHashSet<String>, ids: Collection<String>) {
        into += ids
        while (into.size > capacity) into.remove(into.first())
    }

    private companion object {
        const val DEFAULT_CAPACITY = 256
    }
}

/**
 * The ids of [message] that a push can name: XEP-0359 stanza-ids whose
 * `by` is the entity that archived it for this account, i.e. the account
 * for a 1:1 message and the room for a groupchat one (XEP-0359 §4). Any
 * other `<stanza-id/>` is sender-controlled, so a peer could otherwise
 * plant a pushed message's id and silence its push (§8). Origin and
 * message ids are never matched: push does not carry them.
 */
internal fun trustedIdsOf(message: WaddleMessage, ownBareJid: String?): Set<String> {
    val isGroupchat = message.isMuc || message.messageType == MESSAGE_TYPE_GROUPCHAT
    val authority = if (isGroupchat) message.from?.let(::bareJidOf) else ownBareJid?.let(::bareJidOf)
    authority ?: return emptySet()
    return message.stanzaIds
        .filter { it.by.equals(authority, ignoreCase = true) }
        .mapTo(LinkedHashSet()) { it.id }
}

private const val MESSAGE_TYPE_GROUPCHAT = "groupchat"

/** Where [AnnouncedItems] keeps the ids push posted, across processes. */
interface PushedItemsStore {
    suspend fun load(): List<String>

    suspend fun save(items: List<String>)
}

/** [PushedItemsStore] in the session prefs, so sign-out clears it too. */
class SessionPrefsPushedItems(private val prefs: SessionPrefs) : PushedItemsStore {
    override suspend fun load(): List<String> = prefs.pushAnnouncedItems.first()

    override suspend fun save(items: List<String>) = prefs.setPushAnnouncedItems(items)
}

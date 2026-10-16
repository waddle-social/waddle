package social.waddle.android.client.prefs

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import androidx.datastore.preferences.core.stringSetPreferencesKey
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.map
import kotlinx.serialization.json.Json
import social.waddle.android.client.calls.MucCallSessionCache
import kotlin.random.Random

/**
 * Session-scoped persistence (`session.preferences_pb`), mirroring the
 * web localStorage keys: `waddle.chat.session`, `waddle.chat.joined-rooms`,
 * `waddle.chat.last-seen`,
 * `waddle.chat.outbound-queue`, `waddle.chat.resume-cursors`.
 */
class SessionPrefs(
    private val dataStore: DataStore<Preferences>,
    private val json: Json = Json { ignoreUnknownKeys = true },
) {
    /**
     * MUC group-call session cache sharing this session DataStore file,
     * so [clear] on logout wipes retained call sessions with the rest
     * of the session state (web localStorage parity).
     */
    val mucCallSessions: MucCallSessionCache = MucCallSessionCache(dataStore, json)

    /** Push delivery state, in this file so [clear] wipes it on logout. */
    val push: PushStateStore = PushStateStore(dataStore, json)

    val serverUrl: Flow<String?> = dataStore.data.map { it[KEY_SERVER_URL] }
    val sessionId: Flow<String?> = dataStore.data.map { it[KEY_SESSION_ID] }
    val joinedRooms: Flow<Set<String>> = dataStore.data.map { it[KEY_JOINED_ROOMS] ?: emptySet() }

    val lastSeen: Flow<Map<String, String>> = dataStore.data.map { prefs ->
        prefs[KEY_LAST_SEEN]?.let { stored ->
            runCatching { json.decodeFromString<Map<String, String>>(stored) }.getOrNull()
        } ?: emptyMap()
    }

    val outboundQueue: Flow<List<QueuedOutboundMessage>> = dataStore.data.map { prefs ->
        prefs[KEY_OUTBOUND_QUEUE]?.let(::decodeOutboundQueue) ?: emptyList()
    }

    val resumeCursors: Flow<Map<String, ResumeCursor>> = dataStore.data.map { prefs ->
        prefs[KEY_RESUME_CURSORS]?.let { stored ->
            runCatching { json.decodeFromString<Map<String, ResumeCursor>>(stored) }.getOrNull()
        } ?: emptyMap()
    }

    suspend fun setServerUrl(url: String) {
        dataStore.edit { it[KEY_SERVER_URL] = url }
    }

    /**
     * Bare JID of the signed-in account, persisted so process-death
     * revivals (notification direct replies) can attribute work before
     * the in-memory session manager has restored. Cleared with the rest
     * of the session on logout.
     */
    val ownerBareJid: Flow<String?> = dataStore.data.map { prefs -> prefs[KEY_OWNER_BARE_JID] }

    suspend fun setOwnerBareJid(bareJid: String) {
        dataStore.edit { prefs -> prefs[KEY_OWNER_BARE_JID] = bareJid }
    }

    suspend fun setSessionId(sessionId: String) {
        dataStore.edit { it[KEY_SESSION_ID] = sessionId }
    }

    suspend fun setJoinedRooms(rooms: Set<String>) {
        dataStore.edit { it[KEY_JOINED_ROOMS] = rooms }
    }

    suspend fun setLastSeen(conversationJid: String, marker: String) {
        dataStore.edit { prefs ->
            val current = prefs[KEY_LAST_SEEN]?.let { stored ->
                runCatching { json.decodeFromString<Map<String, String>>(stored) }.getOrNull()
            } ?: emptyMap()
            prefs[KEY_LAST_SEEN] = json.encodeToString(current + (conversationJid to marker))
        }
    }

    /**
     * Atomic read-modify-write of the persisted outbound queue: the
     * DataStore serializes concurrent edits, so enqueue/remove callers
     * need no external lock.
     */
    suspend fun updateOutboundQueue(
        transform: (List<QueuedOutboundMessage>) -> List<QueuedOutboundMessage>,
    ) {
        dataStore.edit { prefs ->
            val next = transform(prefs[KEY_OUTBOUND_QUEUE]?.let(::decodeOutboundQueue) ?: emptyList())
            if (next.isEmpty()) {
                prefs.remove(KEY_OUTBOUND_QUEUE)
            } else {
                prefs[KEY_OUTBOUND_QUEUE] = json.encodeToString(next)
            }
        }
    }

    /**
     * XEP-0359 stanza-ids of messages a push notification already
     * announced, oldest first, so a later process recognises them when the
     * XEP-0160 offline flush delivers them live. Cleared with the session.
     */
    val pushAnnouncedItems: Flow<List<String>> = dataStore.data.map { prefs ->
        prefs[KEY_PUSH_ANNOUNCED_ITEMS]?.let(::decodeStringList).orEmpty()
    }

    suspend fun setPushAnnouncedItems(items: List<String>) {
        dataStore.edit { prefs ->
            if (items.isEmpty()) {
                prefs.remove(KEY_PUSH_ANNOUNCED_ITEMS)
            } else {
                prefs[KEY_PUSH_ANNOUNCED_ITEMS] = json.encodeToString(items)
            }
        }
    }

    suspend fun setResumeCursors(cursors: Map<String, ResumeCursor>) {
        dataStore.edit { prefs ->
            if (cursors.isEmpty()) {
                prefs.remove(KEY_RESUME_CURSORS)
            } else {
                prefs[KEY_RESUME_CURSORS] = json.encodeToString(cursors)
            }
        }
    }

    private fun decodeStringList(stored: String): List<String>? =
        runCatching { json.decodeFromString<List<String>>(stored) }.getOrNull()

    private fun decodeOutboundQueue(stored: String): List<QueuedOutboundMessage>? =
        runCatching { json.decodeFromString<List<QueuedOutboundMessage>>(stored) }.getOrNull()

    /**
     * Stable-per-install 8-hex suffix for the XMPP resource
     * (`waddle-android-<suffix>`); generated once, atomically, and kept
     * across [clear] so reinstalls — not logouts — rotate the resource.
     */
    suspend fun resourceSuffix(): String {
        var suffix = ""
        dataStore.edit { prefs ->
            suffix = prefs[KEY_RESOURCE_SUFFIX] ?: generateResourceSuffix().also {
                prefs[KEY_RESOURCE_SUFFIX] = it
            }
        }
        return suffix
    }

    /** Logout: wipe everything except the per-install resource suffix. */
    suspend fun clear() {
        dataStore.edit { prefs ->
            val suffix = prefs[KEY_RESOURCE_SUFFIX]
            prefs.clear()
            suffix?.let { prefs[KEY_RESOURCE_SUFFIX] = it }
        }
    }

    private fun generateResourceSuffix(): String =
        buildString(RESOURCE_SUFFIX_LENGTH) {
            repeat(RESOURCE_SUFFIX_LENGTH) { append(HEX_ALPHABET.random(Random)) }
        }

    private companion object {
        val KEY_SERVER_URL = stringPreferencesKey("server_url")
        val KEY_SESSION_ID = stringPreferencesKey("session_id")
        val KEY_OWNER_BARE_JID = stringPreferencesKey("owner_bare_jid")
        val KEY_JOINED_ROOMS = stringSetPreferencesKey("joined_rooms")
        val KEY_LAST_SEEN = stringPreferencesKey("last_seen")
        val KEY_OUTBOUND_QUEUE = stringPreferencesKey("outbound_queue")
        val KEY_RESUME_CURSORS = stringPreferencesKey("resume_cursors")
        val KEY_RESOURCE_SUFFIX = stringPreferencesKey("resource_suffix")
        val KEY_PUSH_ANNOUNCED_ITEMS = stringPreferencesKey("push_announced_items")

        const val RESOURCE_SUFFIX_LENGTH = 8
        const val HEX_ALPHABET = "0123456789abcdef"
    }
}

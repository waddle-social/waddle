package social.waddle.android.client.calls

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import kotlinx.coroutines.flow.first
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.longOrNull
import social.waddle.android.client.bareJid
import social.waddle.client.ffi.WaddleCallMedia
import social.waddle.client.ffi.WaddleLiveKitJoin
import java.util.Base64

/**
 * One persisted MUC group-call session (web `CachedMucCallSession`):
 * the per-attempt Jingle sid plus the LiveKit join credentials the SFU
 * minted, so a process death can either resume the call directly
 * (LiveKit identity-uniqueness displaces the orphan session) or send
 * the XEP-0166 terminate the dead resource still owes the mixer.
 */
@Serializable
data class MucCallSessionEntry(
    val roomJid: String,
    val sid: String,
    val selfFullJid: String,
    val updatedAtEpochMillis: Long,
    val mediaAudio: Boolean = true,
    val mediaVideo: Boolean = false,
    val joinUrl: String? = null,
    val joinRoom: String? = null,
    val joinIdentity: String? = null,
    val joinToken: String? = null,
    /** The join token's JWT `exp` claim, when parseable at cache time. */
    val expiresAtEpochMillis: Long? = null,
    /** A leave's terminate failed; retry before reusing this room's slot. */
    val terminatePending: Boolean = false,
) {
    /** Rehydrate the FFI join record; `null` when any credential is missing. */
    fun join(): WaddleLiveKitJoin? {
        val url = joinUrl ?: return null
        val room = joinRoom ?: return null
        val identity = joinIdentity ?: return null
        val token = joinToken ?: return null
        return WaddleLiveKitJoin(url = url, room = room, identity = identity, token = token)
    }

    fun media(): WaddleCallMedia = WaddleCallMedia(audio = mediaAudio, video = mediaVideo)
}

/**
 * DataStore-backed MUC group-call session cache, the Android analog of
 * the web's localStorage `waddle.chat.muc-call-sessions` (see
 * muc-call-session-cache.ts). Persisted in the session preferences
 * file, so logout's [social.waddle.android.client.prefs.SessionPrefs.clear]
 * wipes it with the rest of the session state.
 */
class MucCallSessionCache(
    private val dataStore: DataStore<Preferences>,
    private val json: Json = Json { ignoreUnknownKeys = true },
) {
    /** Remember a freshly-accepted session, replacing the room's previous entry. */
    suspend fun remember(
        roomJid: String,
        sid: String,
        selfFullJid: String?,
        media: WaddleCallMedia,
        join: WaddleLiveKitJoin,
        nowMillis: Long = System.currentTimeMillis(),
    ) {
        val room = normalizeMucCallRoomJid(roomJid)
        val self = selfFullJid?.trim().orEmpty()
        if (room.isEmpty() || sid.isEmpty() || self.isEmpty()) return
        val entry = MucCallSessionEntry(
            roomJid = room,
            sid = sid,
            selfFullJid = self,
            updatedAtEpochMillis = nowMillis,
            mediaAudio = media.audio,
            mediaVideo = media.video,
            joinUrl = join.url,
            joinRoom = join.room,
            joinIdentity = join.identity,
            joinToken = join.token,
            expiresAtEpochMillis = liveKitJoinTokenExpiresAtMillis(join.token),
        )
        update(nowMillis) { entries ->
            listOf(entry) + entries.filterNot { it.roomJid == room && it.selfFullJid == self }
        }
    }

    /** The room's cached session for this identity, or `null` (expired entries excluded). */
    suspend fun read(
        roomJid: String,
        selfFullJid: String?,
        nowMillis: Long = System.currentTimeMillis(),
    ): MucCallSessionEntry? {
        val room = normalizeMucCallRoomJid(roomJid)
        val self = selfFullJid?.trim().orEmpty()
        if (room.isEmpty() || self.isEmpty()) return null
        return load()
            .filterNot { isStale(it, nowMillis) }
            .firstOrNull { it.roomJid == room && it.selfFullJid == self }
    }

    /**
     * Whether the cached session can be resumed directly: identity
     * matches, credentials are complete, no terminate is owed, the
     * entry is fresher than the 24 h window, and any recorded token
     * expiry still has connect headroom.
     */
    suspend fun canResume(
        roomJid: String,
        selfFullJid: String?,
        nowMillis: Long = System.currentTimeMillis(),
    ): Boolean {
        val self = selfFullJid?.trim().orEmpty()
        if (self.isEmpty()) return false
        val entry = read(roomJid, self, nowMillis) ?: return false
        if (entry.terminatePending) return false
        val join = entry.join() ?: return false
        if (join.identity != self) return false
        // No parseable `exp` claim means no connect-headroom proof:
        // refuse and let the caller run a fresh initiate (web parity —
        // `canResumeMucCallActivity` treats an unparseable exp as
        // unusable).
        val expiresAt = entry.expiresAtEpochMillis ?: return false
        return expiresAt > nowMillis + TOKEN_EXPIRY_SKEW_MILLIS
    }

    /**
     * The room's freshest entry owed by this ACCOUNT — any resource
     * sharing [selfFullJid]'s bare JID. The retained-call cleanup path
     * matches on the account because the resource suffix that minted
     * the session may not survive the process death that orphaned it.
     */
    suspend fun retainedEntry(
        roomJid: String,
        selfFullJid: String?,
        nowMillis: Long = System.currentTimeMillis(),
    ): MucCallSessionEntry? {
        val room = normalizeMucCallRoomJid(roomJid)
        val selfBare = bareJid(selfFullJid?.trim().orEmpty()).lowercase()
        if (room.isEmpty() || selfBare.isEmpty()) return null
        return load()
            .filterNot { isStale(it, nowMillis) }
            .firstOrNull { it.roomJid == room && bareJid(it.selfFullJid).lowercase() == selfBare }
    }

    /**
     * Every non-stale `terminatePending` entry owed by this account —
     * the once-per-connect retry set (web
     * `hydrateMucCallTerminatePendingSessions` parity).
     */
    suspend fun terminatePendingEntries(
        selfFullJid: String?,
        nowMillis: Long = System.currentTimeMillis(),
    ): List<MucCallSessionEntry> {
        val selfBare = bareJid(selfFullJid?.trim().orEmpty()).lowercase()
        if (selfBare.isEmpty()) return emptyList()
        return load()
            .filterNot { isStale(it, nowMillis) }
            .filter { it.terminatePending && bareJid(it.selfFullJid).lowercase() == selfBare }
    }

    /**
     * A leave's cached-sid terminate failed on the wire; retain the
     * entry flagged so a later cleanup can retry, and so resume never
     * reuses a session the mixer still considers half-open.
     */
    suspend fun markTerminatePending(
        roomJid: String,
        sid: String,
        selfFullJid: String?,
        nowMillis: Long = System.currentTimeMillis(),
    ) {
        val room = normalizeMucCallRoomJid(roomJid)
        val self = selfFullJid?.trim().orEmpty()
        if (room.isEmpty() || sid.isEmpty() || self.isEmpty()) return
        update(nowMillis) { entries ->
            val existing = entries.any { it.roomJid == room && it.sid == sid && it.selfFullJid == self }
            if (existing) {
                entries.map { entry ->
                    if (entry.roomJid == room && entry.sid == sid && entry.selfFullJid == self) {
                        entry.copy(terminatePending = true, updatedAtEpochMillis = nowMillis)
                    } else {
                        entry
                    }
                }
            } else {
                listOf(
                    MucCallSessionEntry(
                        roomJid = room,
                        sid = sid,
                        selfFullJid = self,
                        updatedAtEpochMillis = nowMillis,
                        terminatePending = true,
                    ),
                ) + entries
            }
        }
    }

    /** Drop the room's entry (optionally only when [sid] matches). */
    suspend fun forget(roomJid: String, selfFullJid: String?, sid: String? = null) {
        val room = normalizeMucCallRoomJid(roomJid)
        val self = selfFullJid?.trim().orEmpty()
        if (room.isEmpty() || self.isEmpty()) return
        update(nowMillis = null) { entries ->
            entries.filterNot { entry ->
                entry.roomJid == room && entry.selfFullJid == self && (sid == null || entry.sid == sid)
            }
        }
    }

    private suspend fun load(): List<MucCallSessionEntry> =
        decode(dataStore.data.first()[KEY_SESSIONS])

    private suspend fun update(
        nowMillis: Long?,
        transform: (List<MucCallSessionEntry>) -> List<MucCallSessionEntry>,
    ) {
        dataStore.edit { prefs ->
            val pruned = decode(prefs[KEY_SESSIONS])
                .filterNot { entry -> nowMillis != null && isStale(entry, nowMillis) }
            val next = transform(pruned).take(MAX_ENTRIES)
            if (next.isEmpty()) {
                prefs.remove(KEY_SESSIONS)
            } else {
                prefs[KEY_SESSIONS] = json.encodeToString(next)
            }
        }
    }

    private fun decode(stored: String?): List<MucCallSessionEntry> =
        stored?.let { raw ->
            runCatching { json.decodeFromString<List<MucCallSessionEntry>>(raw) }.getOrNull()
        } ?: emptyList()

    private fun isStale(entry: MucCallSessionEntry, nowMillis: Long): Boolean =
        nowMillis - entry.updatedAtEpochMillis > CACHE_WINDOW_MILLIS

    companion object {
        /** Entries older than this are unusable (web `CACHE_WINDOW_MS`). */
        const val CACHE_WINDOW_MILLIS = 24L * 60 * 60 * 1000

        /** Headroom the join token needs for the reconnect (web skew). */
        const val TOKEN_EXPIRY_SKEW_MILLIS = 30_000L

        private const val MAX_ENTRIES = 64

        private val KEY_SESSIONS = stringPreferencesKey("muc_call_sessions")
    }
}

/**
 * Decode the LiveKit join JWT's `exp` claim (epoch millis), or `null`
 * for malformed input (web `liveKitJoinTokenExpiresAt`).
 */
internal fun liveKitJoinTokenExpiresAtMillis(token: String): Long? {
    val payload = token.split('.').getOrNull(1) ?: return null
    val decoded = runCatching { Base64.getUrlDecoder().decode(payload) }.getOrNull() ?: return null
    val exp = runCatching {
        Json.parseToJsonElement(String(decoded, Charsets.UTF_8)).jsonObject["exp"]?.jsonPrimitive?.longOrNull
    }.getOrNull()
    return exp?.takeIf { it > 0 }?.times(1000)
}

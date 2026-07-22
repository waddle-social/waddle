package social.waddle.android.client.store

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import social.waddle.android.client.bareJid
import social.waddle.client.ffi.WaddleActivity
import social.waddle.client.ffi.WaddleAvatar
import social.waddle.client.ffi.WaddleMood
import social.waddle.client.ffi.WaddlePepProfile
import social.waddle.client.ffi.WaddleTune
import social.waddle.client.ffi.WaddleVCard4

/**
 * Profile state for one session: the account's own vCard4 (XEP-0292)
 * and PEP status signals (XEP-0107/0108/0118), plus an XEP-0084 avatar
 * cache covering the account AND its peers.
 *
 * The avatar cache is keyed (bare JID → SHA-1 item id → avatar)
 * because XEP-0084 §4.2 forbids re-retrieving image data for an item
 * id that is already held locally; [avatars] separately tracks each
 * JID's CURRENTLY advertised avatar for display.
 */
class ProfileStore {
    private val _selfVcard = MutableStateFlow<WaddleVCard4?>(null)

    /** The account's own published vCard4; `null` until loaded or absent. */
    val selfVcard: StateFlow<WaddleVCard4?> = _selfVcard.asStateFlow()

    private val _selfMood = MutableStateFlow<WaddleMood?>(null)

    /** The account's own published XEP-0107 mood. */
    val selfMood: StateFlow<WaddleMood?> = _selfMood.asStateFlow()

    private val _selfActivity = MutableStateFlow<WaddleActivity?>(null)

    /** The account's own published XEP-0108 activity. */
    val selfActivity: StateFlow<WaddleActivity?> = _selfActivity.asStateFlow()

    private val _selfTune = MutableStateFlow<WaddleTune?>(null)

    /** The account's own published XEP-0118 tune. */
    val selfTune: StateFlow<WaddleTune?> = _selfTune.asStateFlow()

    /** bare JID → (XEP-0084 item id → cached avatar bytes). */
    private val cacheById = MutableStateFlow<Map<String, Map<String, WaddleAvatar>>>(emptyMap())

    private val _avatars = MutableStateFlow<Map<String, WaddleAvatar>>(emptyMap())

    /** bare JID → currently advertised avatar (the account and peers). */
    val avatars: StateFlow<Map<String, WaddleAvatar>> = _avatars.asStateFlow()

    fun setSelfVcard(vcard: WaddleVCard4?) {
        _selfVcard.value = vcard
    }

    fun setSelfMood(mood: WaddleMood?) {
        _selfMood.value = mood
    }

    fun setSelfActivity(activity: WaddleActivity?) {
        _selfActivity.value = activity
    }

    fun setSelfTune(tune: WaddleTune?) {
        _selfTune.value = tune
    }

    /** Seed mood/activity/tune from one fetched PEP profile snapshot. */
    fun setSelfStatus(profile: WaddlePepProfile) {
        _selfMood.value = profile.mood
        _selfActivity.value = profile.activity
        _selfTune.value = profile.tune
    }

    /** The cached avatar for (bare JID, item id), if any — the XEP-0084
     *  §4.2 lookup that gates whether a fetch may touch the wire. */
    fun cachedAvatar(jid: String, itemId: String): WaddleAvatar? =
        cacheById.value[bareJid(jid)]?.get(itemId)

    /** The item ids whose bytes are cached for [jid] — the known-id set
     *  handed to the FFI fetch so the §4.2 data-IQ skip happens on the
     *  wire path, not only on the local shortcut. */
    fun knownAvatarIds(jid: String): List<String> =
        cacheById.value[bareJid(jid)]?.keys?.toList() ?: emptyList()

    /** Record [avatar] as its owner's current avatar and cache its
     *  bytes. The per-JID byte cache is bounded to the
     *  [MAX_CACHED_AVATAR_IDS_PER_JID] most recently seen ids (kept in
     *  insertion order); older entries are evicted. */
    fun onAvatar(avatar: WaddleAvatar) {
        val owner = bareJid(avatar.jid)
        cacheById.update { cache ->
            // Re-insert so the current id is always the newest entry.
            val entries = ((cache[owner] ?: emptyMap()) - avatar.id) + (avatar.id to avatar)
            val bounded = if (entries.size > MAX_CACHED_AVATAR_IDS_PER_JID) {
                entries.entries
                    .drop(entries.size - MAX_CACHED_AVATAR_IDS_PER_JID)
                    .associate { it.key to it.value }
            } else {
                entries
            }
            cache + (owner to bounded)
        }
        _avatars.update { it + (owner to avatar) }
    }

    /** XEP-0084 §4.3 "no avatar": drop the JID's current avatar. The
     *  id-keyed byte cache is kept — a re-published id must still hit it. */
    fun clearAvatar(jid: String) {
        _avatars.update { it - bareJid(jid) }
    }

    fun clear() {
        _selfVcard.value = null
        _selfMood.value = null
        _selfActivity.value = null
        _selfTune.value = null
        cacheById.value = emptyMap()
        _avatars.value = emptyMap()
    }

    companion object {
        /** Modest per-JID byte-cache bound: the current id plus a few
         *  recent ones (an avatar A→B→A flip still skips refetches)
         *  without letting a churn-happy peer grow the cache unbounded. */
        const val MAX_CACHED_AVATAR_IDS_PER_JID = 4
    }
}

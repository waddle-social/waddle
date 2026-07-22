package social.waddle.android.client

import kotlinx.coroutines.CancellationException
import social.waddle.android.client.session.ActiveSession
import social.waddle.android.client.store.SessionStores
import social.waddle.client.ffi.WaddleActivity
import social.waddle.client.ffi.WaddleAvatar
import social.waddle.client.ffi.WaddleClientInterface
import social.waddle.client.ffi.WaddleMood
import social.waddle.client.ffi.WaddleTune
import social.waddle.client.ffi.WaddleVCard4
import java.security.MessageDigest

/** XEP-0084 §3.2: the mandated pubsub item id — SHA-1 hex of the raw
 *  image bytes (mirrors the FFI's `compute_avatar_item_id`). */
internal fun avatarItemId(data: ByteArray): String =
    MessageDigest.getInstance("SHA-1").digest(data)
        .joinToString(separator = "") { byte -> "%02x".format(byte) }

/**
 * Profile-publishing passthroughs (XEP-0292 vCard4, XEP-0084 avatar,
 * XEP-0107/0108/0118 status signals). Same shape rules as
 * [ConversationVerbs]: never throws past the seam — every call
 * collapses to a typed [VerbResult] or `null`, and the profile store
 * stays consistent with what the server accepted.
 */
internal class ProfileVerbs(
    private val activeSession: ActiveSession,
    private val stores: SessionStores,
) {
    /**
     * Load the signed-in account's profile: vCard4, PEP status
     * snapshot, and current avatar. Only the vCard fetch is
     * result-bearing (the UI gates publishing on a successful load,
     * web `VCardEditor` parity); the status and avatar fetches are
     * best-effort — an absent PEP node must not fail the load.
     */
    suspend fun loadSelfProfile(): VerbResult {
        val client = activeSession.client ?: return VerbResult.NotConnected
        val own = activeSession.ownBareJid ?: return VerbResult.NotReady
        val vcard = try {
            client.fetchVcard4(own)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            return VerbResult.Rejected
        }
        stores.profileStore.setSelfVcard(vcard)
        activeSession.fetch { it.fetchUserPepProfile(own) }
            ?.let { stores.profileStore.setSelfStatus(it) }
        fetchAvatar(own)
        return VerbResult.Ok
    }

    /**
     * XEP-0084 avatar fetch honoring §4.2: with a [knownId] whose
     * bytes are already cached, the cached avatar is re-marked current
     * and returned WITHOUT touching the wire (the spec's "MUST NOT
     * retrieve the image data"). Fetched avatars land in the store's
     * (bare JID → item id) cache, peers included.
     */
    suspend fun fetchAvatar(jid: String, knownId: String? = null): WaddleAvatar? {
        val owner = bareJid(jid)
        if (knownId != null) {
            stores.profileStore.cachedAvatar(owner, knownId)?.let { cached ->
                stores.profileStore.onAvatar(cached)
                return cached
            }
        }
        val client = activeSession.client ?: return null
        val avatar = try {
            client.requestAvatar(owner)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            null
        } ?: return null
        stores.profileStore.onAvatar(avatar)
        return avatar
    }

    /**
     * XEP-0292: publish the account's vCard4, applied optimistically —
     * the store shows the new value immediately and rolls back to the
     * previous one when the publish fails (web `VCardEditor` parity).
     * Owner-gated like the reaction rollback: a rollback racing logout
     * must not park pre-logout state into the next session.
     */
    suspend fun publishProfile(vcard: WaddleVCard4): VerbResult {
        val owner = activeSession.ownBareJid ?: return VerbResult.NotReady
        val previous = stores.profileStore.selfVcard.value
        stores.profileStore.setSelfVcard(vcard)
        var result: VerbResult = VerbResult.Rejected
        try {
            result = unitVerb { it.publishVcard4(vcard) }
        } finally {
            if (result != VerbResult.Ok && activeSession.ownBareJid == owner) {
                stores.profileStore.setSelfVcard(previous)
            }
        }
        return result
    }

    /** XEP-0084 §3: publish the account's avatar (data then metadata,
     *  ordered inside the FFI); on success the bytes are cached at
     *  their SHA-1 item id and become the account's current avatar. */
    suspend fun publishAvatar(data: ByteArray, mimeType: String, width: UInt, height: UInt): VerbResult {
        val own = activeSession.ownBareJid ?: return VerbResult.NotReady
        val result = unitVerb { it.publishAvatar(data, mimeType, width, height) }
        if (result == VerbResult.Ok) {
            stores.profileStore.onAvatar(
                WaddleAvatar(jid = own, id = avatarItemId(data), mimeType = mimeType, data = data, url = null),
            )
        }
        return result
    }

    /** XEP-0084 §4.3: publish the empty metadata "no avatar" item. */
    suspend fun disableAvatar(): VerbResult {
        val own = activeSession.ownBareJid ?: return VerbResult.NotReady
        val result = unitVerb { it.disableAvatar() }
        if (result == VerbResult.Ok) {
            stores.profileStore.clearAvatar(own)
        }
        return result
    }

    /** XEP-0107: publish a mood; the store reflects it on success. */
    suspend fun setMood(kind: String, text: String?): VerbResult =
        unitVerb { it.publishMood(kind, text) }.also { result ->
            if (result == VerbResult.Ok) {
                stores.profileStore.setSelfMood(WaddleMood(kind = kind, text = text))
            }
        }

    /** XEP-0107 §2.2: retract the mood via the empty payload. */
    suspend fun clearMood(): VerbResult =
        unitVerb { it.retractMood() }.also { result ->
            if (result == VerbResult.Ok) stores.profileStore.setSelfMood(null)
        }

    /** XEP-0108: publish an activity; the store reflects it on success. */
    suspend fun setActivity(general: String, specific: String?, text: String?): VerbResult =
        unitVerb { it.publishActivity(general, specific, text) }.also { result ->
            if (result == VerbResult.Ok) {
                stores.profileStore.setSelfActivity(
                    WaddleActivity(general = general, specific = specific, text = text),
                )
            }
        }

    /** XEP-0108: retract the activity via the empty payload. */
    suspend fun clearActivity(): VerbResult =
        unitVerb { it.retractActivity() }.also { result ->
            if (result == VerbResult.Ok) stores.profileStore.setSelfActivity(null)
        }

    /** XEP-0118: publish a tune (the debounce lives in the manager). */
    suspend fun publishTune(tune: WaddleTune): VerbResult =
        unitVerb { it.publishTune(tune) }.also { result ->
            if (result == VerbResult.Ok) stores.profileStore.setSelfTune(tune)
        }

    /** XEP-0118 §3.2: stop publishing via the empty payload. */
    suspend fun clearTune(): VerbResult =
        unitVerb { it.retractTune() }.also { result ->
            if (result == VerbResult.Ok) stores.profileStore.setSelfTune(null)
        }

    /** Unit-returning FFI verb shape: the profile publishes signal
     *  refusal by throwing `WaddleException` instead of returning
     *  false, so success is simply "did not throw". */
    private suspend fun unitVerb(op: suspend (WaddleClientInterface) -> Unit): VerbResult {
        val client = activeSession.client ?: return VerbResult.NotConnected
        return try {
            op(client)
            VerbResult.Ok
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            VerbResult.Rejected
        }
    }
}

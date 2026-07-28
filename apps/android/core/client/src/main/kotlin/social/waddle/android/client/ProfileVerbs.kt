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
 *
 * Every write captures one [ActiveSession.OwnerLease] before it starts.
 * FFI use and every optimistic, commit, or rollback store projection are
 * fenced by that same lease, so a slow operation racing a logout or any
 * relogin cannot use or mutate a successor account.
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
        val lease = activeSession.captureOwnerLease() ?: return VerbResult.NotReady
        val own = lease.ownerBareJid
        val vcard = try {
            when (val result = activeSession.invokeIfCurrent(lease) { it.fetchVcard4(own) }) {
                ActiveSession.LeaseInvocation.Stale,
                ActiveSession.LeaseInvocation.NotConnected,
                -> return VerbResult.NotConnected
                is ActiveSession.LeaseInvocation.Completed -> result.value
            }
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            return VerbResult.Rejected
        }
        if (!activeSession.applyIfCurrent(lease) { stores.profileStore.setSelfVcard(vcard) }) {
            return VerbResult.NotConnected
        }
        val status = try {
            when (val result = activeSession.invokeIfCurrent(lease) { it.fetchUserPepProfile(own) }) {
                ActiveSession.LeaseInvocation.Stale,
                ActiveSession.LeaseInvocation.NotConnected,
                -> return VerbResult.NotConnected
                is ActiveSession.LeaseInvocation.Completed -> result.value
            }
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            null
        }
        if (status != null && !activeSession.applyIfCurrent(lease) {
                stores.profileStore.setSelfStatus(status)
            }
        ) {
            return VerbResult.NotConnected
        }
        // Each follow-up remains bound to the original lease. A completed
        // old read cannot issue an avatar IQ through a successor client.
        fetchAvatarForLease(own, lease)
        if (!activeSession.isCurrent(lease)) return VerbResult.NotConnected
        return VerbResult.Ok
    }

    /**
     * XEP-0084 avatar fetch honoring §4.2 for real: the store's known
     * item ids ride along on the FFI call, and when the advertised
     * metadata id is among them the FFI answers id-only WITHOUT the
     * data IQ (the spec's "MUST NOT retrieve the image data") — the
     * cached bytes are then re-marked current. A [knownId] whose bytes
     * are already cached short-circuits without touching the wire at
     * all. Fetched avatars land in the store's (bare JID → item id)
     * cache, peers included.
     */
    suspend fun fetchAvatar(jid: String, knownId: String? = null): WaddleAvatar? {
        val lease = activeSession.captureOwnerLease() ?: return null
        return fetchAvatarForLease(jid, lease, knownId)
    }

    private suspend fun fetchAvatarForLease(
        jid: String,
        lease: ActiveSession.OwnerLease,
        knownId: String? = null,
    ): WaddleAvatar? {
        val owner = bareJid(jid)
        if (knownId != null) {
            stores.profileStore.cachedAvatar(owner, knownId)?.let { cached ->
                return if (activeSession.applyIfCurrent(lease) {
                        stores.profileStore.onAvatar(cached)
                    }
                ) {
                    cached
                } else {
                    null
                }
            }
        }
        val knownIds = stores.profileStore.knownAvatarIds(owner)
        val result = try {
            when (val invocation = activeSession.invokeIfCurrent(lease) { it.requestAvatar(owner, knownIds) }) {
                ActiveSession.LeaseInvocation.Stale,
                ActiveSession.LeaseInvocation.NotConnected,
                -> return null
                is ActiveSession.LeaseInvocation.Completed -> invocation.value
            }
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            null
        } ?: return null
        // An id-only result means the FFI skipped the data fetch: the
        // bytes for that id are, by construction, in the cache we
        // handed it — re-mark them current.
        val avatar = result.avatar
            ?: stores.profileStore.cachedAvatar(owner, result.id)
            ?: return null
        return if (activeSession.applyIfCurrent(lease) { stores.profileStore.onAvatar(avatar) }) avatar else null
    }

    /**
     * XEP-0292: publish the account's vCard4, applied optimistically —
     * the store shows the new value immediately and rolls back to the
     * previous one when the publish fails (web `VCardEditor` parity).
     * Lease-gated like every other write: a rollback racing a logout or
     * relogin must not park pre-logout state into the next session.
     */
    suspend fun publishProfile(vcard: WaddleVCard4): VerbResult {
        val lease = activeSession.captureOwnerLease() ?: return VerbResult.NotReady
        return publishProfileWithLease(lease, vcard)
    }

    /**
     * Exact-lease implementation of [publishProfile]. Keeping this separate
     * lets the lifecycle suite prove that an operation parked with a retired
     * lease cannot select or mutate a successor account.
     */
    internal suspend fun publishProfileWithLease(
        lease: ActiveSession.OwnerLease,
        vcard: WaddleVCard4,
    ): VerbResult {
        // No live client → nothing to publish, and more importantly no
        // optimistic write: inside login()'s bump→clear window the
        // client is provably null, and capturing `previous` there would
        // let the rollback resurrect the OLD account's vCard into the
        // freshly seeded stores.
        if (!hasLiveClient(lease)) return VerbResult.NotConnected
        var previous: WaddleVCard4? = null
        if (!activeSession.applyIfCurrent(lease) {
                previous = stores.profileStore.selfVcard.value
                stores.profileStore.setSelfVcard(vcard)
            }
        ) {
            return VerbResult.NotConnected
        }
        val result = unitVerb(lease) { it.publishVcard4(vcard) }
        if (result == VerbResult.Ok) {
            // A completed old call must not be reported as committed after
            // its account attempt has been retired.
            return if (activeSession.applyIfCurrent(lease) {}) VerbResult.Ok else VerbResult.NotConnected
        }
        // The optimistic value belongs only to this exact attempt.  A
        // stale rollback must not overwrite a successor's profile.
        activeSession.applyIfCurrent(lease) { stores.profileStore.setSelfVcard(previous) }
        return result
    }

    /** XEP-0084 §3: publish the account's avatar (data then metadata,
     *  ordered inside the FFI); on success the bytes are cached at
     *  their SHA-1 item id and become the account's current avatar. */
    suspend fun publishAvatar(data: ByteArray, mimeType: String, width: UInt, height: UInt): VerbResult {
        val lease = activeSession.captureOwnerLease() ?: return VerbResult.NotReady
        val result = unitVerb(lease) { it.publishAvatar(data, mimeType, width, height) }
        if (result == VerbResult.Ok && !activeSession.applyIfCurrent(lease) {
            stores.profileStore.onAvatar(
                WaddleAvatar(
                    jid = lease.ownerBareJid,
                    id = avatarItemId(data),
                    mimeType = mimeType,
                    data = data,
                    url = null,
                ),
            )
        }
        ) {
            return VerbResult.NotConnected
        }
        return result
    }

    /** XEP-0084 §4.3: publish the empty metadata "no avatar" item. */
    suspend fun disableAvatar(): VerbResult {
        val lease = activeSession.captureOwnerLease() ?: return VerbResult.NotReady
        val result = unitVerb(lease) { it.disableAvatar() }
        if (result == VerbResult.Ok && !activeSession.applyIfCurrent(lease) {
                stores.profileStore.clearAvatar(lease.ownerBareJid)
            }
        ) {
            return VerbResult.NotConnected
        }
        return result
    }

    /** XEP-0107: publish a mood; the store reflects it on success. */
    suspend fun setMood(kind: String, text: String?): VerbResult {
        val lease = activeSession.captureOwnerLease() ?: return VerbResult.NotConnected
        val result = unitVerb(lease) { it.publishMood(kind, text) }
        if (result == VerbResult.Ok && !activeSession.applyIfCurrent(lease) {
                stores.profileStore.setSelfMood(WaddleMood(kind = kind, text = text))
            }
        ) {
            return VerbResult.NotConnected
        }
        return result
    }

    /** XEP-0107 §2.2: retract the mood via the empty payload. */
    suspend fun clearMood(): VerbResult {
        val lease = activeSession.captureOwnerLease() ?: return VerbResult.NotConnected
        val result = unitVerb(lease) { it.retractMood() }
        if (result == VerbResult.Ok && !activeSession.applyIfCurrent(lease) {
                stores.profileStore.setSelfMood(null)
            }
        ) {
            return VerbResult.NotConnected
        }
        return result
    }

    /** XEP-0108: publish an activity; the store reflects it on success. */
    suspend fun setActivity(general: String, specific: String?, text: String?): VerbResult {
        val lease = activeSession.captureOwnerLease() ?: return VerbResult.NotConnected
        val result = unitVerb(lease) { it.publishActivity(general, specific, text) }
        if (result == VerbResult.Ok && !activeSession.applyIfCurrent(lease) {
                stores.profileStore.setSelfActivity(
                    WaddleActivity(general = general, specific = specific, text = text),
                )
            }
        ) {
            return VerbResult.NotConnected
        }
        return result
    }

    /** XEP-0108: retract the activity via the empty payload. */
    suspend fun clearActivity(): VerbResult {
        val lease = activeSession.captureOwnerLease() ?: return VerbResult.NotConnected
        val result = unitVerb(lease) { it.retractActivity() }
        if (result == VerbResult.Ok && !activeSession.applyIfCurrent(lease) {
                stores.profileStore.setSelfActivity(null)
            }
        ) {
            return VerbResult.NotConnected
        }
        return result
    }

    /** XEP-0118: publish a tune; the store reflects it on success. */
    suspend fun publishTune(tune: WaddleTune): VerbResult {
        val lease = activeSession.captureOwnerLease() ?: return VerbResult.NotConnected
        val result = unitVerb(lease) { it.publishTune(tune) }
        if (result == VerbResult.Ok && !activeSession.applyIfCurrent(lease) {
                stores.profileStore.setSelfTune(tune)
            }
        ) {
            return VerbResult.NotConnected
        }
        return result
    }

    /** XEP-0118 §3.2: stop publishing via the empty payload. */
    suspend fun clearTune(): VerbResult {
        val lease = activeSession.captureOwnerLease() ?: return VerbResult.NotConnected
        val result = unitVerb(lease) { it.retractTune() }
        if (result == VerbResult.Ok && !activeSession.applyIfCurrent(lease) {
                stores.profileStore.setSelfTune(null)
            }
        ) {
            return VerbResult.NotConnected
        }
        return result
    }

    /** Unit-returning FFI verb shape: the profile publishes signal
     *  refusal by throwing `WaddleException` instead of returning
     *  false, so success is simply "did not throw". */
    /** Confirm that an optimistic projection has a live owner-bound client. */
    private suspend fun hasLiveClient(lease: ActiveSession.OwnerLease): Boolean = when (
        activeSession.invokeIfCurrent(lease) { Unit }
    ) {
        ActiveSession.LeaseInvocation.Stale,
        ActiveSession.LeaseInvocation.NotConnected,
        -> false
        is ActiveSession.LeaseInvocation.Completed -> true
    }

    private suspend fun unitVerb(
        lease: ActiveSession.OwnerLease,
        op: suspend (WaddleClientInterface) -> Unit,
    ): VerbResult {
        return try {
            when (activeSession.invokeIfCurrent(lease, op)) {
                ActiveSession.LeaseInvocation.Stale,
                ActiveSession.LeaseInvocation.NotConnected,
                -> return VerbResult.NotConnected
                is ActiveSession.LeaseInvocation.Completed -> Unit
            }
            VerbResult.Ok
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            VerbResult.Rejected
        }
    }
}

package social.waddle.android.client

import kotlinx.coroutines.CancellationException
import social.waddle.android.client.session.ActiveSession
import social.waddle.android.client.store.SessionStores
import social.waddle.client.ffi.WaddleException
import social.waddle.client.ffi.WaddleStickerPack

/**
 * XEP-0449 sticker-pack passthroughs over the account's own PEP
 * `urn:xmpp:stickers:0` node. Same shape rules as [ConversationVerbs]:
 * never throws past the seam — every call collapses to a typed result,
 * and accepted mutations reconcile the session cache
 * ([SessionStores.stickerPackStore]) so the picker updates without a
 * refetch.
 */
internal class StickerVerbs(
    private val activeSession: ActiveSession,
    private val stores: SessionStores,
) {
    /**
     * Load the account's own packs into the store. Session-cached: a
     * previous Ready/Empty answer short-circuits (publish/retract keep
     * it current); Unavailable retries — the picker reopening after a
     * reconnect must not stay stuck on a stale failure.
     */
    suspend fun loadStickerPacks() {
        if (stores.stickerPackStore.isLoaded) return
        val lease = activeSession.captureOwnerLease() ?: return
        loadStickerPacks(lease)
    }

    /** Exact-attempt entry point for the ready/catch-up pipeline and tests. */
    internal suspend fun loadStickerPacks(lease: ActiveSession.OwnerLease) {
        if (stores.stickerPackStore.isLoaded) return
        val packs = try {
            when (val result = activeSession.invokeIfCurrent(lease) { it.fetchStickerPacks(null) }) {
                ActiveSession.LeaseInvocation.Stale,
                ActiveSession.LeaseInvocation.NotConnected,
                -> {
                    activeSession.applyIfCurrent(lease) { stores.stickerPackStore.applyUnavailable() }
                    return
                }
                is ActiveSession.LeaseInvocation.Completed -> result.value
            }
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            activeSession.applyIfCurrent(lease) { stores.stickerPackStore.applyUnavailable() }
            return
        }
        activeSession.applyIfCurrent(lease) { stores.stickerPackStore.applyLoaded(packs.map { it.toDomain() }) }
    }

    /**
     * Publish a new pack of [items]. The FFI recomputes the pack id
     * from content; the store upserts under that server-derived id on
     * Ok. An empty item list is a local precondition failure — the
     * FFI would reject it as InvalidArgument anyway.
     */
    suspend fun publishPack(name: String, summary: String?, items: List<StickerItem>): VerbResult {
        if (items.isEmpty()) return VerbResult.NotReady
        val lease = activeSession.captureOwnerLease() ?: return VerbResult.NotReady
        val pack = WaddleStickerPack(
            // Display-state only: the FFI derives the real id from content.
            id = "",
            name = name.trim().takeIf { it.isNotEmpty() },
            summary = summary?.trim()?.takeIf { it.isNotEmpty() },
            restricted = false,
            stickers = items.map { it.toFfi() },
        )
        val packId = try {
            when (val result = activeSession.invokeIfCurrent(lease) { it.publishStickerPack(pack) }) {
                ActiveSession.LeaseInvocation.Stale,
                ActiveSession.LeaseInvocation.NotConnected,
                -> return VerbResult.NotConnected
                is ActiveSession.LeaseInvocation.Completed -> result.value
            }
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: WaddleException.NotConnected) {
            return VerbResult.NotConnected
        } catch (_: Throwable) {
            return VerbResult.Rejected
        }
        return if (activeSession.applyIfCurrent(lease) {
                stores.stickerPackStore.applyPublished(
                    StickerPack(
                        id = packId,
                        name = pack.name,
                        summary = pack.summary,
                        restricted = false,
                        stickers = items,
                    ),
                )
            }
        ) VerbResult.Ok else VerbResult.NotConnected
    }

    /** Retract the pack item [packId]; drops it from the store on Ok. */
    suspend fun removePack(packId: String): VerbResult {
        val lease = activeSession.captureOwnerLease() ?: return VerbResult.NotReady
        val retracted = try {
            when (val result = activeSession.invokeIfCurrent(lease) { it.retractStickerPack(packId) }) {
                ActiveSession.LeaseInvocation.Stale,
                ActiveSession.LeaseInvocation.NotConnected,
                -> return VerbResult.NotConnected
                is ActiveSession.LeaseInvocation.Completed -> result.value
            }
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: WaddleException.NotConnected) {
            return VerbResult.NotConnected
        } catch (_: Throwable) {
            return VerbResult.Rejected
        }
        if (!retracted) return VerbResult.Rejected
        return if (activeSession.applyIfCurrent(lease) {
                stores.stickerPackStore.applyRemoved(packId)
            }
        ) VerbResult.Ok else VerbResult.NotConnected
    }
}

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
        val packs = try {
            when (val result = activeSession.invoke { it.fetchStickerPacks(null) }) {
                ActiveSession.Invocation.NotConnected -> {
                    stores.stickerPackStore.applyUnavailable()
                    return
                }
                is ActiveSession.Invocation.Completed -> result.value
            }
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            stores.stickerPackStore.applyUnavailable()
            return
        }
        stores.stickerPackStore.applyLoaded(packs.map { it.toDomain() })
    }

    /**
     * Publish a new pack of [items]. The FFI recomputes the pack id
     * from content; the store upserts under that server-derived id on
     * Ok. An empty item list is a local precondition failure — the
     * FFI would reject it as InvalidArgument anyway.
     */
    suspend fun publishPack(name: String, summary: String?, items: List<StickerItem>): VerbResult {
        if (items.isEmpty()) return VerbResult.NotReady
        val pack = WaddleStickerPack(
            // Display-state only: the FFI derives the real id from content.
            id = "",
            name = name.trim().takeIf { it.isNotEmpty() },
            summary = summary?.trim()?.takeIf { it.isNotEmpty() },
            restricted = false,
            stickers = items.map { it.toFfi() },
        )
        val packId = try {
            when (val result = activeSession.invoke { it.publishStickerPack(pack) }) {
                ActiveSession.Invocation.NotConnected -> return VerbResult.NotConnected
                is ActiveSession.Invocation.Completed -> result.value
            }
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: WaddleException.NotConnected) {
            return VerbResult.NotConnected
        } catch (_: Throwable) {
            return VerbResult.Rejected
        }
        stores.stickerPackStore.applyPublished(
            StickerPack(
                id = packId,
                name = pack.name,
                summary = pack.summary,
                restricted = false,
                stickers = items,
            ),
        )
        return VerbResult.Ok
    }

    /** Retract the pack item [packId]; drops it from the store on Ok. */
    suspend fun removePack(packId: String): VerbResult {
        val retracted = try {
            when (val result = activeSession.invoke { it.retractStickerPack(packId) }) {
                ActiveSession.Invocation.NotConnected -> return VerbResult.NotConnected
                is ActiveSession.Invocation.Completed -> result.value
            }
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: WaddleException.NotConnected) {
            return VerbResult.NotConnected
        } catch (_: Throwable) {
            return VerbResult.Rejected
        }
        if (!retracted) return VerbResult.Rejected
        stores.stickerPackStore.applyRemoved(packId)
        return VerbResult.Ok
    }
}

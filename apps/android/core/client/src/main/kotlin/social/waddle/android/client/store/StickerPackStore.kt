package social.waddle.android.client.store

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import social.waddle.android.client.StickerPack
import social.waddle.android.client.StickerPacksResult

/**
 * Session cache of the account's own XEP-0449 sticker packs: seeded by
 * one `fetch_sticker_packs` per session, kept current by post-Ok
 * publish/retract updates (the PEP node has no live notification path
 * wired yet, and only this client mutates the own node).
 */
class StickerPackStore {
    private val _packs = MutableStateFlow<StickerPacksResult?>(null)

    /** `null` until the first load of the session. */
    val packs: StateFlow<StickerPacksResult?> = _packs.asStateFlow()

    /** True when a cached answer exists (a load can skip the fetch). */
    val isLoaded: Boolean
        get() = when (_packs.value) {
            is StickerPacksResult.Ready, StickerPacksResult.Empty -> true
            else -> false
        }

    /** A fetch answered: cache its packs (empty = the first-run state). */
    fun applyLoaded(packs: List<StickerPack>) {
        _packs.value = if (packs.isEmpty()) StickerPacksResult.Empty else StickerPacksResult.Ready(packs)
    }

    /** The fetch failed or no session was live. */
    fun applyUnavailable() {
        _packs.value = StickerPacksResult.Unavailable
    }

    /** A publish was accepted: upsert the pack under its server-derived id. */
    fun applyPublished(pack: StickerPack) {
        _packs.update { current ->
            val existing = (current as? StickerPacksResult.Ready)?.packs.orEmpty()
            StickerPacksResult.Ready(existing.filterNot { it.id == pack.id } + pack)
        }
    }

    /** A retract was accepted: drop the pack; none left = [StickerPacksResult.Empty]. */
    fun applyRemoved(packId: String) {
        _packs.update { current ->
            val remaining = (current as? StickerPacksResult.Ready)?.packs.orEmpty()
                .filterNot { it.id == packId }
            if (remaining.isEmpty()) StickerPacksResult.Empty else StickerPacksResult.Ready(remaining)
        }
    }

    fun clear() {
        _packs.value = null
    }
}

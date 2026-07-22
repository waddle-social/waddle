package social.waddle.android.feature.conversation

import social.waddle.android.client.StickerPack

/**
 * Pure view-state of the sticker picker's Ready branch, extracted for
 * JVM testing: which pack the grid shows and whether the pack-chip row
 * renders at all.
 */
data class StickerPickerViewState(
    /** Chip row renders only when there is a choice to make. */
    val showPackChips: Boolean,
    /** The pack whose stickers fill the grid; null only for no packs. */
    val selectedPack: StickerPack?,
)

/**
 * Resolve the grid's pack: the explicitly selected id when it still
 * exists (a removed pack falls back gracefully), else the first pack.
 */
fun stickerPickerViewState(packs: List<StickerPack>, selectedPackId: String?): StickerPickerViewState =
    StickerPickerViewState(
        showPackChips = packs.size > 1,
        selectedPack = packs.firstOrNull { it.id == selectedPackId } ?: packs.firstOrNull(),
    )

/** Chip/dialog label of a pack: its name, or the id for unnamed packs. */
fun stickerPackLabel(pack: StickerPack): String = pack.name?.takeIf { it.isNotBlank() } ?: pack.id

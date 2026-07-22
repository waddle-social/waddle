package social.waddle.android.feature.conversation

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.StickerPack

class StickerPickerStateTest {
    private val packA = StickerPack(id = "a", name = "Penguins")
    private val packB = StickerPack(id = "b", name = null)

    @Test
    fun `a single pack hides the chip row and fills the grid`() {
        val state = stickerPickerViewState(listOf(packA), selectedPackId = null)
        assertFalse(state.showPackChips)
        assertEquals(packA, state.selectedPack)
    }

    @Test
    fun `multiple packs show chips and honor the selection`() {
        val state = stickerPickerViewState(listOf(packA, packB), selectedPackId = "b")
        assertTrue(state.showPackChips)
        assertEquals(packB, state.selectedPack)
    }

    @Test
    fun `a removed selection falls back to the first pack`() {
        val state = stickerPickerViewState(listOf(packA, packB), selectedPackId = "gone")
        assertEquals(packA, state.selectedPack)
    }

    @Test
    fun `no packs resolve to no selection`() {
        assertNull(stickerPickerViewState(emptyList(), selectedPackId = null).selectedPack)
    }

    @Test
    fun `pack labels fall back to the id for unnamed packs`() {
        assertEquals("Penguins", stickerPackLabel(packA))
        assertEquals("b", stickerPackLabel(packB))
        assertEquals("c", stickerPackLabel(StickerPack(id = "c", name = "  ")))
    }
}

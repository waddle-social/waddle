package social.waddle.android.feature.conversation

import androidx.activity.ComponentActivity
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import social.waddle.android.client.StickerHash
import social.waddle.android.client.StickerItem
import social.waddle.android.client.StickerPack
import social.waddle.android.client.StickerPacksResult

/** Sticker picker sheet: empty-state CTA, pack grid, and selection. */
@RunWith(AndroidJUnit4::class)
class StickerPickerSheetTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private val penguin = StickerItem(
        desc = "🐧",
        mediaType = "image/webp",
        sizeBytes = 1234L,
        width = 512,
        height = 512,
        hashes = listOf(StickerHash(algo = "sha-256", valueB64 = "aGFzaA==")),
        sources = listOf("https://upload.waddle.test/penguin.webp"),
    )

    private val pack = StickerPack(
        id = "pack-1",
        name = "Penguins",
        summary = null,
        restricted = false,
        stickers = listOf(penguin),
    )

    @Test
    fun emptyStateShowsTheCreatePackCallToAction() {
        var createTapped = 0
        composeRule.setContent {
            StickerPickerSheet(
                packs = StickerPacksResult.Empty,
                onSelect = { _, _ -> },
                onCreatePack = { createTapped += 1 },
                onRemovePack = {},
                onDismiss = {},
            )
        }

        composeRule.waitUntil(TIMEOUT_MS) {
            runCatching {
                composeRule.onNodeWithText("You have no sticker packs yet.").assertExists()
                true
            }.getOrDefault(false)
        }
        composeRule.onNodeWithTag(StickerPickerTestTags.CREATE_CTA).performClick()
        composeRule.runOnIdle { assertEquals(1, createTapped) }
    }

    @Test
    fun selectingAStickerEmitsTheItemAndItsPack() {
        var selected: Pair<StickerItem, StickerPack>? = null
        composeRule.setContent {
            StickerPickerSheet(
                packs = StickerPacksResult.Ready(listOf(pack)),
                onSelect = { item, fromPack -> selected = item to fromPack },
                onCreatePack = {},
                onRemovePack = {},
                onDismiss = {},
            )
        }

        composeRule.waitUntil(TIMEOUT_MS) {
            runCatching {
                composeRule.onNodeWithTag(StickerPickerTestTags.GRID).assertExists()
                true
            }.getOrDefault(false)
        }
        composeRule.onNodeWithTag(StickerPickerTestTags.sticker(0)).performClick()
        composeRule.runOnIdle {
            assertEquals(penguin to pack, selected)
        }
    }

    @Test
    fun removePackAsksForConfirmationBeforeEmitting() {
        var removed: StickerPack? = null
        composeRule.setContent {
            StickerPickerSheet(
                packs = StickerPacksResult.Ready(listOf(pack)),
                onSelect = { _, _ -> },
                onCreatePack = {},
                onRemovePack = { removed = it },
                onDismiss = {},
            )
        }

        composeRule.waitUntil(TIMEOUT_MS) {
            runCatching {
                composeRule.onNodeWithTag(StickerPickerTestTags.PACK_MENU).assertExists()
                true
            }.getOrDefault(false)
        }
        composeRule.onNodeWithTag(StickerPickerTestTags.PACK_MENU).performClick()
        composeRule.onNodeWithTag(StickerPickerTestTags.REMOVE_ACTION).performClick()
        composeRule.runOnIdle { assertEquals(null, removed) }
        composeRule.onNodeWithTag(StickerPickerTestTags.REMOVE_CONFIRM).performClick()
        composeRule.runOnIdle { assertEquals(pack, removed) }
    }

    private companion object {
        const val TIMEOUT_MS = 5_000L
    }
}

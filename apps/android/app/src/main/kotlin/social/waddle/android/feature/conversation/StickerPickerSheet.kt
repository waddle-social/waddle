package social.waddle.android.feature.conversation

import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.itemsIndexed
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import coil3.compose.AsyncImage
import social.waddle.android.R
import social.waddle.android.client.StickerItem
import social.waddle.android.client.StickerPack
import social.waddle.android.client.StickerPacksResult

/**
 * XEP-0449 sticker picker bottom sheet (the GIF picker's sibling): the
 * user's own packs' stickers in a grid, a pack-chip row when more than
 * one pack exists, an empty state with a create-pack call to action,
 * and a per-pack remove action behind a confirm dialog. Selecting a
 * sticker emits the item + its pack; the caller sends it.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun StickerPickerSheet(
    packs: StickerPacksResult?,
    onSelect: (StickerItem, StickerPack) -> Unit,
    onCreatePack: () -> Unit,
    onRemovePack: (StickerPack) -> Unit,
    onDismiss: () -> Unit,
) {
    var selectedPackId by remember { mutableStateOf<String?>(null) }
    var removeTarget by remember { mutableStateOf<StickerPack?>(null) }

    ModalBottomSheet(
        onDismissRequest = onDismiss,
        modifier = Modifier.testTag(StickerPickerTestTags.SHEET),
    ) {
        Column(
            modifier = Modifier
                .navigationBarsPadding()
                .padding(horizontal = 16.dp)
                .padding(bottom = 8.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                text = stringResource(R.string.sticker_picker_title),
                style = MaterialTheme.typography.titleMedium,
            )
            when (packs) {
                null -> StickerStatusText(stringResource(R.string.sticker_picker_loading))
                StickerPacksResult.Unavailable ->
                    StickerStatusText(stringResource(R.string.sticker_picker_unavailable))
                StickerPacksResult.Empty -> StickerEmptyState(onCreatePack)
                is StickerPacksResult.Ready -> StickerPackContent(
                    packs = packs.packs,
                    selectedPackId = selectedPackId,
                    onSelectPack = { selectedPackId = it },
                    onSelect = onSelect,
                    onCreatePack = onCreatePack,
                    onRemovePack = { removeTarget = it },
                )
            }
        }
    }

    removeTarget?.let { pack ->
        AlertDialog(
            onDismissRequest = { removeTarget = null },
            title = { Text(text = stringResource(R.string.sticker_remove_confirm_title)) },
            text = {
                Text(
                    text = stringResource(R.string.sticker_remove_confirm_body, stickerPackLabel(pack)),
                )
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        onRemovePack(pack)
                        removeTarget = null
                    },
                    modifier = Modifier.testTag(StickerPickerTestTags.REMOVE_CONFIRM),
                ) {
                    Text(
                        text = stringResource(R.string.sticker_remove_pack),
                        color = MaterialTheme.colorScheme.error,
                    )
                }
            },
            dismissButton = {
                TextButton(onClick = { removeTarget = null }) {
                    Text(text = stringResource(R.string.action_cancel))
                }
            },
        )
    }
}

@Composable
private fun StickerEmptyState(onCreatePack: () -> Unit) {
    Column(
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(8.dp),
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 24.dp),
    ) {
        Text(
            text = stringResource(R.string.sticker_picker_empty),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Button(
            onClick = onCreatePack,
            modifier = Modifier.testTag(StickerPickerTestTags.CREATE_CTA),
        ) {
            Text(text = stringResource(R.string.sticker_create_pack))
        }
    }
}

@Composable
private fun StickerPackContent(
    packs: List<StickerPack>,
    selectedPackId: String?,
    onSelectPack: (String) -> Unit,
    onSelect: (StickerItem, StickerPack) -> Unit,
    onCreatePack: () -> Unit,
    onRemovePack: (StickerPack) -> Unit,
) {
    val viewState = stickerPickerViewState(packs, selectedPackId)
    val pack = viewState.selectedPack ?: return
    if (viewState.showPackChips) {
        Row(
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            modifier = Modifier
                .fillMaxWidth()
                .horizontalScroll(rememberScrollState()),
        ) {
            packs.forEach { candidate ->
                FilterChip(
                    selected = candidate.id == pack.id,
                    onClick = { onSelectPack(candidate.id) },
                    label = { Text(text = stickerPackLabel(candidate)) },
                    modifier = Modifier.testTag(StickerPickerTestTags.packChip(candidate.id)),
                )
            }
        }
    }
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Text(
            text = stickerPackLabel(pack),
            style = MaterialTheme.typography.labelLarge,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.weight(1f),
        )
        TextButton(
            onClick = onCreatePack,
            modifier = Modifier.testTag(StickerPickerTestTags.CREATE_CTA),
        ) {
            Text(text = stringResource(R.string.sticker_create_pack))
        }
        StickerPackOverflow(pack, onRemovePack)
    }
    StickerGrid(pack, onSelect)
}

@Composable
private fun StickerPackOverflow(pack: StickerPack, onRemovePack: (StickerPack) -> Unit) {
    var menuOpen by remember { mutableStateOf(false) }
    IconButton(
        onClick = { menuOpen = true },
        modifier = Modifier.testTag(StickerPickerTestTags.PACK_MENU),
    ) {
        Icon(
            Icons.Filled.MoreVert,
            contentDescription = stringResource(R.string.sticker_pack_actions),
        )
    }
    DropdownMenu(expanded = menuOpen, onDismissRequest = { menuOpen = false }) {
        DropdownMenuItem(
            text = { Text(text = stringResource(R.string.sticker_remove_pack)) },
            onClick = {
                menuOpen = false
                onRemovePack(pack)
            },
            modifier = Modifier.testTag(StickerPickerTestTags.REMOVE_ACTION),
        )
    }
}

@Composable
private fun StickerGrid(pack: StickerPack, onSelect: (StickerItem, StickerPack) -> Unit) {
    LazyVerticalGrid(
        columns = GridCells.Fixed(STICKER_GRID_COLUMNS),
        horizontalArrangement = Arrangement.spacedBy(4.dp),
        verticalArrangement = Arrangement.spacedBy(4.dp),
        modifier = Modifier
            .heightIn(max = 360.dp)
            .testTag(StickerPickerTestTags.GRID),
    ) {
        itemsIndexed(pack.stickers) { index, sticker ->
            AsyncImage(
                model = sticker.sources.firstOrNull(),
                contentDescription = sticker.desc,
                contentScale = ContentScale.Fit,
                modifier = Modifier
                    .aspectRatio(1f)
                    .clip(RoundedCornerShape(6.dp))
                    .testTag(StickerPickerTestTags.sticker(index))
                    .clickable { onSelect(sticker, pack) },
            )
        }
    }
}

@Composable
private fun StickerStatusText(text: String) {
    Text(
        text = text,
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(vertical = 24.dp),
    )
}

private const val STICKER_GRID_COLUMNS = 4

/** Semantics tags for the sticker picker, shared with tests. */
object StickerPickerTestTags {
    const val SHEET = "sticker-picker-sheet"
    const val CREATE_CTA = "sticker-picker-create"
    const val GRID = "sticker-picker-grid"
    const val PACK_MENU = "sticker-picker-pack-menu"
    const val REMOVE_ACTION = "sticker-picker-remove"
    const val REMOVE_CONFIRM = "sticker-picker-remove-confirm"

    fun packChip(packId: String) = "sticker-picker-chip-$packId"

    fun sticker(index: Int) = "sticker-picker-item-$index"
}

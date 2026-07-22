package social.waddle.android.feature.conversation

import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import coil3.compose.AsyncImage
import kotlinx.coroutines.launch
import social.waddle.android.R

/** XEP-0449 anti-DoS cap: at most 24 stickers per published pack. */
const val STICKER_PACK_MAX_ITEMS = 24

/**
 * Create-pack bottom sheet: pack name + summary, 1–24 images via the
 * photo picker, an optional lang-less desc per image (blank falls back
 * to the pack name at publish), upload progress, and inline failure
 * states. [onCreate] runs the full pipeline ([StickerPackCreator]);
 * the sheet only owns form + progress state.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun CreateStickerPackSheet(
    onCreate: suspend (
        name: String,
        summary: String?,
        images: List<StickerImageInput>,
        onProgress: (Int, Int) -> Unit,
    ) -> CreateStickerPackResult,
    onCreated: () -> Unit,
    onDismiss: () -> Unit,
) {
    var name by remember { mutableStateOf("") }
    var summary by remember { mutableStateOf("") }
    var images by remember { mutableStateOf(listOf<StickerImageInput>()) }
    var creating by remember { mutableStateOf(false) }
    var progress by remember { mutableStateOf(0 to 0) }
    var failure by remember { mutableStateOf<CreateStickerPackResult?>(null) }
    val scope = rememberCoroutineScope()
    val imagePicker = rememberLauncherForActivityResult(
        ActivityResultContracts.PickMultipleVisualMedia(STICKER_PACK_MAX_ITEMS),
    ) { uris ->
        val known = images.map { it.uri }.toSet()
        images = (images + uris.filterNot { it in known }.map { StickerImageInput(it, "") })
            .take(STICKER_PACK_MAX_ITEMS)
    }

    ModalBottomSheet(
        // A mid-upload dismiss would silently drop the half-created
        // pack; the sheet stays until the attempt resolves.
        onDismissRequest = { if (!creating) onDismiss() },
        modifier = Modifier.testTag(CreateStickerPackTestTags.SHEET),
    ) {
        Column(
            modifier = Modifier
                .navigationBarsPadding()
                .padding(horizontal = 16.dp)
                .padding(bottom = 8.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                text = stringResource(R.string.sticker_create_pack),
                style = MaterialTheme.typography.titleMedium,
            )
            OutlinedTextField(
                value = name,
                onValueChange = { name = it },
                label = { Text(text = stringResource(R.string.sticker_pack_name)) },
                singleLine = true,
                enabled = !creating,
                modifier = Modifier
                    .fillMaxWidth()
                    .testTag(CreateStickerPackTestTags.NAME_FIELD),
            )
            OutlinedTextField(
                value = summary,
                onValueChange = { summary = it },
                label = { Text(text = stringResource(R.string.sticker_pack_summary)) },
                singleLine = true,
                enabled = !creating,
                modifier = Modifier.fillMaxWidth(),
            )
            TextButton(
                onClick = {
                    imagePicker.launch(
                        PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly),
                    )
                },
                enabled = !creating && images.size < STICKER_PACK_MAX_ITEMS,
                modifier = Modifier.testTag(CreateStickerPackTestTags.ADD_IMAGES),
            ) {
                Text(text = stringResource(R.string.sticker_add_images))
            }
            if (images.isNotEmpty()) {
                PickedStickerImages(
                    images = images,
                    enabled = !creating,
                    onDescChanged = { index, desc ->
                        images = images.mapIndexed { i, input ->
                            if (i == index) input.copy(desc = desc) else input
                        }
                    },
                    onRemove = { index ->
                        images = images.filterIndexed { i, _ -> i != index }
                    },
                )
            }
            if (creating) {
                Text(
                    text = stringResource(
                        R.string.sticker_create_progress,
                        progress.first,
                        progress.second,
                    ),
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.primary,
                )
            }
            failure?.let { result ->
                Text(
                    text = stringResource(
                        if (result == CreateStickerPackResult.NotConnected) {
                            R.string.sticker_create_offline
                        } else {
                            R.string.sticker_create_failed
                        },
                    ),
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.error,
                    modifier = Modifier.testTag(CreateStickerPackTestTags.FAILURE),
                )
            }
            Button(
                onClick = {
                    creating = true
                    failure = null
                    progress = 0 to images.size
                    scope.launch {
                        val result = runCatching {
                            onCreate(name, summary.trim().takeIf { it.isNotEmpty() }, images) { done, total ->
                                progress = done to total
                            }
                        }.getOrDefault(CreateStickerPackResult.Failed)
                        creating = false
                        if (result == CreateStickerPackResult.Ok) onCreated() else failure = result
                    }
                },
                enabled = !creating && name.isNotBlank() && images.isNotEmpty(),
                modifier = Modifier.testTag(CreateStickerPackTestTags.PUBLISH),
            ) {
                Text(text = stringResource(R.string.sticker_publish_pack))
            }
        }
    }
}

@Composable
private fun PickedStickerImages(
    images: List<StickerImageInput>,
    enabled: Boolean,
    onDescChanged: (Int, String) -> Unit,
    onRemove: (Int) -> Unit,
) {
    LazyColumn(
        verticalArrangement = Arrangement.spacedBy(4.dp),
        modifier = Modifier.heightIn(max = 240.dp),
    ) {
        itemsIndexed(images, key = { _, input -> input.uri }) { index, input ->
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                modifier = Modifier.fillMaxWidth(),
            ) {
                AsyncImage(
                    model = input.uri,
                    contentDescription = null,
                    contentScale = ContentScale.Crop,
                    modifier = Modifier
                        .size(48.dp)
                        .clip(RoundedCornerShape(6.dp)),
                )
                OutlinedTextField(
                    value = input.desc,
                    onValueChange = { onDescChanged(index, it) },
                    placeholder = { Text(text = stringResource(R.string.sticker_desc_placeholder)) },
                    singleLine = true,
                    enabled = enabled,
                    modifier = Modifier.weight(1f),
                )
                IconButton(onClick = { onRemove(index) }, enabled = enabled) {
                    Icon(
                        Icons.Filled.Close,
                        contentDescription = stringResource(R.string.sticker_remove_image),
                    )
                }
            }
        }
    }
}

/** Semantics tags for the create-pack sheet, shared with tests. */
object CreateStickerPackTestTags {
    const val SHEET = "sticker-create-sheet"
    const val NAME_FIELD = "sticker-create-name"
    const val ADD_IMAGES = "sticker-create-add-images"
    const val PUBLISH = "sticker-create-publish"
    const val FAILURE = "sticker-create-failure"
}

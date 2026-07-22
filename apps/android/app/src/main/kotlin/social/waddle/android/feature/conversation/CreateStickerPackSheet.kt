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
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.listSaver
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.core.net.toUri
import coil3.compose.AsyncImage
import social.waddle.android.R

/**
 * Waddle product cap (anti-DoS): at most 24 stickers per published
 * pack. XEP-0449 itself defines no item limit — this bounds the
 * process/upload pipeline and the published pack size by choice.
 */
const val STICKER_PACK_MAX_ITEMS = 24

/**
 * Create-pack bottom sheet: pack name + summary, 1–24 images via the
 * photo picker, an optional lang-less desc per image (blank falls back
 * to the pack name at publish), upload progress, and inline failure
 * states. The create attempt itself lives in [StickerPacksViewModel]
 * ([phase] observes it) so a rotation mid-upload cannot kill the
 * pipeline; the sheet owns only form state, saved across recreation.
 * [onCreated] fires once [CreatePackPhase.Succeeded] is observed.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun CreateStickerPackSheet(
    phase: CreatePackPhase,
    onCreate: (name: String, summary: String?, images: List<StickerImageInput>) -> Unit,
    onCreated: () -> Unit,
    onDismiss: () -> Unit,
) {
    var name by rememberSaveable { mutableStateOf("") }
    var summary by rememberSaveable { mutableStateOf("") }
    var images by rememberSaveable(stateSaver = StickerImageInputListSaver) {
        mutableStateOf(listOf<StickerImageInput>())
    }
    val creating = phase is CreatePackPhase.Creating
    val imagePicker = rememberLauncherForActivityResult(
        ActivityResultContracts.PickMultipleVisualMedia(STICKER_PACK_MAX_ITEMS),
    ) { uris ->
        val known = images.map { it.uri }.toSet()
        images = (images + uris.filterNot { it in known }.map { StickerImageInput(it, "") })
            .take(STICKER_PACK_MAX_ITEMS)
    }

    // Sticky success consumed on (re)composition: even when the sheet
    // was recreated mid-attempt, the finished create closes it exactly
    // once.
    LaunchedEffect(phase) {
        if (phase == CreatePackPhase.Succeeded) onCreated()
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
            (phase as? CreatePackPhase.Creating)?.let { progress ->
                Text(
                    text = stringResource(
                        R.string.sticker_create_progress,
                        progress.done,
                        progress.total,
                    ),
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.primary,
                )
            }
            (phase as? CreatePackPhase.Failed)?.let { failed ->
                Text(
                    text = stringResource(
                        if (failed.result == CreateStickerPackResult.NotConnected) {
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
                    onCreate(name, summary.trim().takeIf { it.isNotEmpty() }, images)
                },
                enabled = !creating && name.isNotBlank() && images.isNotEmpty(),
                modifier = Modifier.testTag(CreateStickerPackTestTags.PUBLISH),
            ) {
                Text(text = stringResource(R.string.sticker_publish_pack))
            }
        }
    }
}

/**
 * Saver for the picked-image form state: flattened (uri, desc) string
 * pairs — `Uri` itself is not saveable-friendly, its string form
 * round-trips losslessly.
 */
private val StickerImageInputListSaver = listSaver<List<StickerImageInput>, String>(
    save = { inputs -> inputs.flatMap { listOf(it.uri.toString(), it.desc) } },
    restore = { flat ->
        flat.chunked(2).mapNotNull { pair ->
            val uri = pair.getOrNull(0) ?: return@mapNotNull null
            StickerImageInput(uri.toUri(), pair.getOrNull(1).orEmpty())
        }
    },
)

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

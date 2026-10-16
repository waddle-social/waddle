package social.waddle.android.feature.conversation

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material3.Icon
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import coil3.compose.AsyncImage
import kotlinx.coroutines.delay
import social.waddle.android.R
import social.waddle.android.client.GIF_SEARCH_DEBOUNCE_MS
import social.waddle.android.client.GifResult
import social.waddle.android.client.GifSearchGateway
import social.waddle.android.client.GifSearchResult
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.huddle.SkeletonBox

/**
 * GIF picker bottom sheet (web `GifPicker.vue` parity): trending on
 * open, 300 ms debounced search, three-column grid of previews, the
 * web's not-configured / rate-limited / unavailable states, and the
 * mandatory "Powered by GIPHY" attribution. Selecting a GIF emits its
 * `original.url`; the caller sends it as a plain message body.
 */
@Composable
fun GifPickerSheet(
    gateway: GifSearchGateway,
    onSelect: (String) -> Unit,
    onDismiss: () -> Unit,
) {
    var query by remember { mutableStateOf("") }
    var loading by remember { mutableStateOf(true) }
    var result by remember { mutableStateOf<GifSearchResult?>(null) }

    LaunchedEffect(query) {
        if (query.isNotBlank()) delay(GIF_SEARCH_DEBOUNCE_MS)
        loading = true
        result = gateway.search(query)
        loading = false
    }

    ConversationSheet(onDismissRequest = onDismiss, title = stringResource(R.string.gif_picker_title)) {
        OutlinedTextField(
            value = query,
            onValueChange = { query = it },
            placeholder = { Text(text = stringResource(R.string.gif_search_placeholder)) },
            leadingIcon = { Icon(Icons.Outlined.Search, contentDescription = null) },
            singleLine = true,
            shape = RoundedCornerShape(Waddle.radius.control),
            modifier = Modifier.fillMaxWidth(),
        )
        GifPickerContent(
            loading = loading,
            trending = query.isBlank(),
            result = result,
            onSelect = onSelect,
        )
        Text(
            text = stringResource(R.string.gif_powered_by),
            style = Waddle.type.time,
            color = Waddle.colors.fgMuted,
        )
    }
}

@Composable
private fun GifPickerContent(
    loading: Boolean,
    trending: Boolean,
    result: GifSearchResult?,
    onSelect: (String) -> Unit,
) {
    when {
        loading || result == null -> PickerGridSkeleton(
            stringResource(if (trending) R.string.gif_loading_trending else R.string.gif_loading),
            columns = GRID_COLUMNS,
        )
        result is GifSearchResult.NotConfigured -> PickerStatusText(stringResource(R.string.gif_not_configured))
        result is GifSearchResult.RateLimited -> PickerStatusText(stringResource(R.string.gif_rate_limited))
        result is GifSearchResult.Unavailable -> PickerStatusText(stringResource(R.string.gif_unavailable))
        result is GifSearchResult.Results && result.gifs.isEmpty() ->
            PickerStatusText(stringResource(R.string.gif_no_results))
        result is GifSearchResult.Results -> GifGrid(result.gifs, onSelect)
    }
}

/** A centred status line for a picker sheet. */
@Composable
internal fun PickerStatusText(text: String) {
    Text(
        text = text,
        style = Waddle.type.control,
        color = Waddle.colors.fgMuted,
        textAlign = TextAlign.Center,
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = Waddle.space.lg),
    )
}

/** Square placeholder cells while a picker grid loads; [label] is what a screen reader hears. */
@Composable
internal fun PickerGridSkeleton(label: String, columns: Int) {
    Column(
        verticalArrangement = Arrangement.spacedBy(Waddle.space.xxs),
        modifier = Modifier.semantics(mergeDescendants = true) { contentDescription = label },
    ) {
        repeat(SKELETON_ROWS) {
            Row(horizontalArrangement = Arrangement.spacedBy(Waddle.space.xxs)) {
                repeat(columns) {
                    SkeletonBox(
                        modifier = Modifier
                            .weight(1f)
                            .aspectRatio(1f),
                        radius = Waddle.radius.control,
                    )
                }
            }
        }
    }
}

@Composable
private fun GifGrid(gifs: List<GifResult>, onSelect: (String) -> Unit) {
    LazyVerticalGrid(
        columns = GridCells.Fixed(GRID_COLUMNS),
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.xxs),
        verticalArrangement = Arrangement.spacedBy(Waddle.space.xxs),
        modifier = Modifier.heightIn(max = GRID_MAX_HEIGHT),
    ) {
        items(gifs, key = { it.id }) { gif ->
            val label = gif.title.ifEmpty { stringResource(R.string.gif_picker_title) }
            AsyncImage(
                model = gif.previewUrl,
                contentDescription = label,
                contentScale = ContentScale.Crop,
                modifier = Modifier
                    .aspectRatio(1f)
                    .clip(RoundedCornerShape(Waddle.radius.control))
                    .clickable(onClickLabel = label) { onSelect(gif.originalUrl) },
            )
        }
    }
}

internal val GRID_MAX_HEIGHT = 360.dp
private const val GRID_COLUMNS = 3
private const val SKELETON_ROWS = 2

package social.waddle.android.feature.conversation

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
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
import androidx.compose.ui.unit.dp
import coil3.compose.AsyncImage
import kotlinx.coroutines.delay
import social.waddle.android.R
import social.waddle.android.client.GIF_SEARCH_DEBOUNCE_MS
import social.waddle.android.client.GifResult
import social.waddle.android.client.GifSearchGateway
import social.waddle.android.client.GifSearchResult

/**
 * GIF picker bottom sheet (web `GifPicker.vue` parity): trending on
 * open, 300 ms debounced search, three-column grid of previews, the
 * web's not-configured / rate-limited / unavailable states, and the
 * mandatory "Powered by GIPHY" attribution. Selecting a GIF emits its
 * `original.url`; the caller sends it as a plain message body.
 */
@OptIn(ExperimentalMaterial3Api::class)
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

    ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(
            modifier = Modifier
                .navigationBarsPadding()
                .padding(horizontal = 16.dp)
                .padding(bottom = 8.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                text = stringResource(R.string.gif_picker_title),
                style = MaterialTheme.typography.titleMedium,
            )
            OutlinedTextField(
                value = query,
                onValueChange = { query = it },
                placeholder = { Text(text = stringResource(R.string.gif_search_placeholder)) },
                singleLine = true,
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
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
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
        loading || result == null -> StatusText(
            stringResource(if (trending) R.string.gif_loading_trending else R.string.gif_loading),
        )
        result is GifSearchResult.NotConfigured -> StatusText(stringResource(R.string.gif_not_configured))
        result is GifSearchResult.RateLimited -> StatusText(stringResource(R.string.gif_rate_limited))
        result is GifSearchResult.Unavailable -> StatusText(stringResource(R.string.gif_unavailable))
        result is GifSearchResult.Results && result.gifs.isEmpty() ->
            StatusText(stringResource(R.string.gif_no_results))
        result is GifSearchResult.Results -> GifGrid(result.gifs, onSelect)
    }
}

@Composable
private fun StatusText(text: String) {
    Text(
        text = text,
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(vertical = 24.dp),
    )
}

@Composable
private fun GifGrid(gifs: List<GifResult>, onSelect: (String) -> Unit) {
    LazyVerticalGrid(
        columns = GridCells.Fixed(GRID_COLUMNS),
        horizontalArrangement = Arrangement.spacedBy(4.dp),
        verticalArrangement = Arrangement.spacedBy(4.dp),
        modifier = Modifier.heightIn(max = 360.dp),
    ) {
        items(gifs, key = { it.id }) { gif ->
            AsyncImage(
                model = gif.previewUrl,
                contentDescription = gif.title.ifEmpty { stringResource(R.string.gif_picker_title) },
                contentScale = ContentScale.Crop,
                modifier = Modifier
                    .aspectRatio(1f)
                    .clip(RoundedCornerShape(6.dp))
                    .clickable { onSelect(gif.originalUrl) },
            )
        }
    }
}

private const val GRID_COLUMNS = 3

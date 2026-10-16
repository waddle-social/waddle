package social.waddle.android.feature.search

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material3.Icon
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.withFrameNanos
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import social.waddle.android.R
import social.waddle.android.feature.conversation.AuthorStyle
import social.waddle.android.feature.conversation.ConversationSheet
import social.waddle.android.feature.conversation.parseInstant
import social.waddle.android.feature.conversation.rememberConversationClock
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.huddle.AvatarSize
import social.waddle.android.ui.huddle.HuddleAvatar
import social.waddle.android.ui.huddle.SkeletonBox

/**
 * Full-text search over one conversation's archive: an autofocused
 * query field above the debounced result list. Results are ephemeral
 * previews (author, time, the body with the match picked out) — the
 * sheet never touches the timeline.
 */
@Composable
fun MessageSearchSheet(
    viewModel: MessageSearchViewModel,
    onDismiss: () -> Unit,
) {
    val query by viewModel.query.collectAsStateWithLifecycle()
    val state by viewModel.state.collectAsStateWithLifecycle()
    val focusRequester = remember { FocusRequester() }
    ConversationSheet(onDismissRequest = onDismiss) {
        OutlinedTextField(
            value = query,
            onValueChange = viewModel::onQueryChanged,
            // Label, not just placeholder: TalkBack keeps announcing
            // the field's purpose after text is typed (web parity
            // with the input's aria-label).
            label = { Text(stringResource(R.string.search_messages)) },
            placeholder = { Text(stringResource(R.string.search_placeholder)) },
            leadingIcon = { Icon(Icons.Outlined.Search, contentDescription = null) },
            singleLine = true,
            shape = RoundedCornerShape(Waddle.radius.control),
            modifier = Modifier
                .fillMaxWidth()
                .focusRequester(focusRequester),
        )
        // Web parity: the caret drops straight into the field on
        // open — without this the user taps the input a second time.
        // The frame yield lets the sheet's dialog-hosted content
        // attach the focus target first; requesting focus on a
        // not-yet-attached node throws.
        LaunchedEffect(Unit) {
            withFrameNanos {}
            focusRequester.requestFocus()
        }
        SearchStateContent(state, query)
    }
}

@Composable
private fun SearchStateContent(state: MessageSearchState, query: String) {
    when (state) {
        MessageSearchState.Idle -> SearchStatusText(stringResource(R.string.search_hint))
        MessageSearchState.Searching -> SearchingRows()
        MessageSearchState.Empty -> SearchStatusText(stringResource(R.string.search_no_results))
        MessageSearchState.Failed -> SearchStatusText(stringResource(R.string.search_failed), failed = true)
        is MessageSearchState.Results -> LazyColumn {
            items(state.hits, key = { it.key }) { hit ->
                SearchResultRow(hit, query)
            }
        }
    }
}

/** Loading state: result-shaped placeholders, so the request reads as in flight. */
@Composable
private fun SearchingRows() {
    val searching = stringResource(R.string.search_searching)
    Column(modifier = Modifier.semantics(mergeDescendants = true) { contentDescription = searching }) {
        repeat(SKELETON_ROWS) {
            Row(
                horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
                modifier = Modifier.padding(vertical = Waddle.space.xs),
            ) {
                SkeletonBox(modifier = Modifier.size(AvatarSize.MD.dp), radius = AvatarSize.MD.dp / 2)
                Column(modifier = Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(Waddle.space.xs)) {
                    SkeletonBox(
                        modifier = Modifier
                            .fillMaxWidth(NAME_FRACTION)
                            .height(SKELETON_LINE),
                    )
                    SkeletonBox(
                        modifier = Modifier
                            .fillMaxWidth()
                            .height(SKELETON_LINE),
                    )
                }
            }
        }
    }
}

/** Centered status line (idle hint, empty, failed). */
@Composable
private fun SearchStatusText(text: String, failed: Boolean = false) {
    Box(
        contentAlignment = Alignment.Center,
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = Waddle.space.lg),
    ) {
        Text(
            text = text,
            style = Waddle.type.control,
            color = if (failed) Waddle.colors.dangerText else Waddle.colors.fgMuted,
            textAlign = TextAlign.Center,
        )
    }
}

/** Result preview: face, author and stamp, two lines of body with the match picked out. */
@Composable
private fun SearchResultRow(hit: MessageSearchHit, query: String) {
    val colors = Waddle.colors
    val clock = rememberConversationClock()
    val author = hit.author ?: stringResource(R.string.message_unknown_sender)
    val highlight = SpanStyle(background = colors.actionSubtle, color = colors.fg, fontWeight = FontWeight.SemiBold)
    Row(
        verticalAlignment = Alignment.Top,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
        modifier = Modifier
            .fillMaxWidth()
            .semantics(mergeDescendants = true) {}
            .padding(vertical = Waddle.space.xs),
    ) {
        HuddleAvatar(jid = hit.authorJid, name = author, size = AvatarSize.MD, ground = colors.surface)
        Column(verticalArrangement = Arrangement.spacedBy(Waddle.space.xxs), modifier = Modifier.weight(1f)) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs),
            ) {
                Text(
                    text = author,
                    style = AuthorStyle,
                    color = colors.fg,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f, fill = false),
                )
                hit.timestamp?.let(::parseInstant)?.let { at ->
                    Text(text = clock.stamp(at), style = Waddle.type.time, color = colors.fgMuted, maxLines = 1)
                }
            }
            Text(
                text = highlighted(hit.body, query, highlight),
                style = Waddle.type.control.copy(fontWeight = FontWeight.Normal),
                color = colors.fgMuted,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}

/** [body] with every case-insensitive occurrence of [query] in [style]. */
internal fun highlighted(body: String, query: String, style: SpanStyle): AnnotatedString {
    val needle = query.trim()
    if (needle.isEmpty()) return AnnotatedString(body)
    return buildAnnotatedString {
        var from = 0
        while (from < body.length) {
            val at = body.indexOf(needle, from, ignoreCase = true)
            if (at < 0) break
            append(body.substring(from, at))
            withStyle(style) { append(body.substring(at, at + needle.length)) }
            from = at + needle.length
        }
        append(body.substring(from))
    }
}

private const val SKELETON_ROWS = 3
private const val NAME_FRACTION = 0.4f
private val SKELETON_LINE = 10.dp

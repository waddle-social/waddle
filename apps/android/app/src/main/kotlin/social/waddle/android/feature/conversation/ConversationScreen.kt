package social.waddle.android.feature.conversation

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.style.TextOverflow
import androidx.lifecycle.compose.LifecycleResumeEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import social.waddle.android.R
import social.waddle.android.client.store.TimelineItem

/**
 * Shared conversation scaffold (channel + DM): top bar, timeline,
 * composer. Marks the conversation active (unread clearing) while
 * resumed.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ConversationScreen(
    title: String,
    viewModel: ConversationViewModel,
    onBack: () -> Unit,
) {
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    val typing by viewModel.typing.collectAsStateWithLifecycle()
    val composerMode by viewModel.composerMode.collectAsStateWithLifecycle()
    val clipboard = LocalClipboardManager.current
    var sheetTarget by remember { mutableStateOf<TimelineItem?>(null) }

    LifecycleResumeEffect(viewModel) {
        viewModel.onConversationVisible()
        onPauseOrDispose { viewModel.onConversationHidden() }
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(text = title, maxLines = 1, overflow = TextOverflow.Ellipsis)
                },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(
                            Icons.AutoMirrored.Filled.ArrowBack,
                            contentDescription = stringResource(R.string.action_back),
                        )
                    }
                },
            )
        },
    ) { padding ->
        Column(
            modifier = Modifier
                .padding(padding)
                .consumeWindowInsets(padding)
                .fillMaxSize()
                .imePadding(),
        ) {
            TimelineList(
                rows = state.rows,
                isLoadingOlder = state.isLoadingOlder,
                onTopReached = viewModel::loadOlder,
                onRetry = viewModel::retry,
                modifier = Modifier.weight(1f),
                pinnedIds = state.pinnedIds,
                onLongPress = { item -> sheetTarget = item },
                onToggleReaction = viewModel::toggleReaction,
            )
            TypingIndicator(names = typing)
            MessageComposer(
                onSend = viewModel::send,
                onDraftChanged = viewModel::onDraftChanged,
                editing = composerMode as? ComposerMode.Editing,
                onCancelEdit = viewModel::cancelEdit,
            )
        }
    }

    sheetTarget?.let { item ->
        MessageActionSheet(
            item = item,
            actionable = viewModel.actionTargetIdOf(item) != null,
            canPin = state.canPin,
            isPinned = item.stanzaId?.let { it in state.pinnedIds } == true,
            onDismiss = { sheetTarget = null },
            onReact = { emoji -> viewModel.toggleReaction(item, emoji) },
            onEdit = { viewModel.startEdit(item) },
            onRetract = { viewModel.retract(item) },
            onCopy = { clipboard.setText(AnnotatedString(item.body)) },
            onSetPinned = { pinned -> viewModel.setPinned(item, pinned) },
        )
    }
}

package social.waddle.android.feature.conversation

import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.outlined.Chat
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.LifecycleResumeEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import social.waddle.android.R
import social.waddle.android.client.VerbResult
import social.waddle.android.client.store.TimelineItem

/**
 * Shared conversation scaffold (channel + DM + thread): top bar,
 * timeline, composer. Marks the conversation active (unread clearing)
 * while resumed. [onOpenThread] is null on thread screens — threads do
 * not nest, so reply-count chips and the overview affordance hide.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ConversationScreen(
    title: String,
    viewModel: ConversationViewModel,
    onBack: () -> Unit,
    onOpenThread: ((threadId: String) -> Unit)? = null,
    /** Own bare JID for the self-mention row highlight (XEP-0372). */
    selfBareJid: String? = null,
) {
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    val typing by viewModel.typing.collectAsStateWithLifecycle()
    val mentionCandidates by viewModel.mentionCandidates.collectAsStateWithLifecycle()
    val composerMode by viewModel.composerMode.collectAsStateWithLifecycle()
    val uploadState by viewModel.uploadState.collectAsStateWithLifecycle()
    val clipboard = LocalClipboardManager.current
    val attachmentPicker = rememberLauncherForActivityResult(
        ActivityResultContracts.GetContent(),
    ) { uri -> uri?.let(viewModel::sendAttachment) }
    var sheetTarget by remember { mutableStateOf<TimelineItem?>(null) }
    var threadsOverviewOpen by remember { mutableStateOf(false) }
    val snackbarHostState = remember { SnackbarHostState() }
    val actionFailedText = stringResource(R.string.action_failed)
    val actionFailedOfflineText = stringResource(R.string.action_failed_offline)

    LaunchedEffect(viewModel) {
        viewModel.actionFailures.collect { failure ->
            snackbarHostState.showSnackbar(
                if (failure is VerbResult.NotConnected) actionFailedOfflineText else actionFailedText,
            )
        }
    }

    LifecycleResumeEffect(viewModel) {
        viewModel.onConversationVisible()
        onPauseOrDispose { viewModel.onConversationHidden() }
    }

    Scaffold(
        snackbarHost = { SnackbarHost(snackbarHostState) },
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
                actions = {
                    if (onOpenThread != null && state.threads.isNotEmpty()) {
                        IconButton(onClick = { threadsOverviewOpen = true }) {
                            Icon(
                                Icons.AutoMirrored.Outlined.Chat,
                                contentDescription = stringResource(R.string.threads_overview_title),
                            )
                        }
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
                threadReplyCounts = state.threadReplyCounts,
                onLongPress = { item -> sheetTarget = item },
                onToggleReaction = viewModel::toggleReaction,
                onOpenThread = onOpenThread?.let { open ->
                    {
                        item ->
                            open(viewModel.threadIdFor(item))
                        }
                },
                onAtNewestEdgeChanged = viewModel::onAtNewestEdgeChanged,
                selfBareJid = selfBareJid,
            )
            TypingIndicator(names = typing)
            MessageComposer(
                onSend = viewModel::send,
                mentionCandidates = mentionCandidates,
                onDraftChanged = viewModel::onDraftChanged,
                editing = composerMode as? ComposerMode.Editing,
                onCancelEdit = viewModel::cancelEdit,
                replying = composerMode as? ComposerMode.Replying,
                onCancelReply = viewModel::cancelReply,
                onAttach = { attachmentPicker.launch("*/*") },
                uploadState = uploadState,
                onClearUpload = viewModel::clearUploadState,
            )
        }
    }

    sheetTarget?.let { item ->
        MessageActionSheet(
            item = item,
            actionable = viewModel.actionTargetIdOf(item) != null,
            canPin = state.canPin,
            isPinned = item.identityIds.any { it in state.pinnedIds },
            onDismiss = { sheetTarget = null },
            onReact = { emoji -> viewModel.toggleReaction(item, emoji) },
            onReply = { viewModel.startReply(item) },
            onReplyInThread = onOpenThread?.let { open ->
                {
                    open(viewModel.threadIdFor(item))
                }
            },
            onEdit = { viewModel.startEdit(item) },
            onRetract = { viewModel.retract(item) },
            onCopy = { clipboard.setText(AnnotatedString(item.body)) },
            onSetPinned = { pinned -> viewModel.setPinned(item, pinned) },
        )
    }

    if (threadsOverviewOpen && onOpenThread != null) {
        ModalBottomSheet(onDismissRequest = { threadsOverviewOpen = false }) {
            Column(modifier = Modifier.navigationBarsPadding()) {
                Text(
                    text = stringResource(R.string.threads_overview_title),
                    style = MaterialTheme.typography.titleMedium,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
                )
                // Scrollable: long histories can hold more threads than
                // the sheet's height.
                LazyColumn {
                    items(state.threads, key = { it.threadId }) { thread ->
                        ListItem(
                            headlineContent = {
                                Text(
                                    text = if (thread.rootTombstoned) {
                                        stringResource(R.string.message_deleted)
                                    } else {
                                        thread.rootPreview
                                    },
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis,
                                )
                            },
                            supportingContent = {
                                Text(
                                    text = pluralStringResource(
                                        R.plurals.thread_replies_count,
                                        thread.replyCount,
                                        thread.replyCount,
                                        thread.rootAuthor ?: "",
                                    ),
                                )
                            },
                            colors = ListItemDefaults.colors(containerColor = Color.Transparent),
                            modifier = Modifier.clickable {
                                threadsOverviewOpen = false
                                onOpenThread(thread.threadId)
                            },
                        )
                    }
                }
            }
        }
    }
}

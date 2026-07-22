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
import androidx.compose.material.icons.outlined.Call
import androidx.compose.material.icons.outlined.Notifications
import androidx.compose.material.icons.outlined.NotificationsOff
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material.icons.outlined.Videocam
import androidx.compose.material3.AlertDialog
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
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleResumeEffect
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.repeatOnLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import kotlinx.coroutines.flow.filterNotNull
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.client.VerbResult
import social.waddle.android.client.store.TimelineItem
import social.waddle.android.feature.search.MessageSearchSheet
import social.waddle.android.feature.search.MessageSearchTarget
import social.waddle.android.feature.search.MessageSearchViewModel
import social.waddle.client.ffi.WaddleNotifyMode

/**
 * Shared conversation scaffold (channel + DM + thread): top bar,
 * timeline, composer. Marks the conversation active (unread clearing)
 * while resumed. [onOpenThread] is null on thread screens — threads do
 * not nest, so reply-count chips and the overview affordance hide.
 * [searchTarget] names the archive the top-bar search action queries;
 * null (thread screens) hides the affordance.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ConversationScreen(
    title: String,
    viewModel: ConversationViewModel,
    onBack: () -> Unit,
    onOpenThread: ((threadId: String) -> Unit)? = null,
    searchTarget: MessageSearchTarget? = null,
    /** Own bare JID for the self-mention row highlight (XEP-0372). */
    selfBareJid: String? = null,
    /** Presence line under the title (XEP-0319 idle, DM screens). */
    subtitle: String? = null,
    /** Origin whose cached XEP-0363 preview images may load. */
    trustedMediaOrigin: String? = null,
    /** Extra host-specific top-bar actions (room members/settings). */
    extraTopBarActions: @Composable () -> Unit = {},
    /** DM-only call entry points; `null` (channels, threads) hides them. */
    onStartCall: ((video: Boolean) -> Unit)? = null,
    /** Host slot rendered above the timeline (channel call banner). */
    aboveTimeline: @Composable () -> Unit = {},
    /** Slash-command wiring; `null` (scaffold tests) hides the feature. */
    slashCommandHost: SlashCommandHost? = null,
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
    val authorPresence by viewModel.authorPresence.collectAsStateWithLifecycle()
    var sheetTarget by remember { mutableStateOf<TimelineItem?>(null) }
    var moderateTarget by remember { mutableStateOf<TimelineItem?>(null) }
    var threadsOverviewOpen by remember { mutableStateOf(false) }
    var notifySheetOpen by remember { mutableStateOf(false) }
    var gifPickerOpen by remember { mutableStateOf(false) }
    // Saveable: the create pipeline runs in a ViewModel and survives a
    // config change — the sheets observing it must come back too.
    var stickerPickerOpen by rememberSaveable { mutableStateOf(false) }
    var createStickerPackOpen by rememberSaveable { mutableStateOf(false) }
    val notifyMode by viewModel.notifyMode.collectAsStateWithLifecycle()
    var searchOpen by remember { mutableStateOf(false) }
    val snackbarHostState = remember { SnackbarHostState() }
    val actionFailedText = stringResource(R.string.action_failed)
    val actionFailedOfflineText = stringResource(R.string.action_failed_offline)
    // Carrier-aware XEP-0060 precondition-not-met copy (web parity):
    // a room's shared bookmark node needs a server admin, a DM's
    // personal PEP node does not.
    val notifyMismatchText = stringResource(
        if (viewModel.isGroupchat) R.string.notify_mismatch_room else R.string.notify_mismatch_dm,
    )

    LaunchedEffect(viewModel) {
        viewModel.actionFailures.collect { failure ->
            snackbarHostState.showSnackbar(
                if (failure is VerbResult.NotConnected) actionFailedOfflineText else actionFailedText,
            )
        }
    }

    LaunchedEffect(viewModel) {
        viewModel.notifySettingsMismatch.collect {
            snackbarHostState.showSnackbar(notifyMismatchText)
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
                    Column {
                        Text(text = title, maxLines = 1, overflow = TextOverflow.Ellipsis)
                        subtitle?.let {
                            Text(
                                text = it,
                                style = MaterialTheme.typography.labelSmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                            )
                        }
                    }
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
                    extraTopBarActions()
                    if (onStartCall != null) {
                        IconButton(
                            onClick = { onStartCall(false) },
                            modifier = Modifier.testTag(ConversationCallTestTags.AUDIO_CALL_BUTTON),
                        ) {
                            Icon(
                                Icons.Outlined.Call,
                                contentDescription = stringResource(R.string.call_start_audio),
                            )
                        }
                        IconButton(
                            onClick = { onStartCall(true) },
                            modifier = Modifier.testTag(ConversationCallTestTags.VIDEO_CALL_BUTTON),
                        ) {
                            Icon(
                                Icons.Outlined.Videocam,
                                contentDescription = stringResource(R.string.call_start_video),
                            )
                        }
                    }
                    if (onOpenThread != null && state.threads.isNotEmpty()) {
                        IconButton(onClick = { threadsOverviewOpen = true }) {
                            Icon(
                                Icons.AutoMirrored.Outlined.Chat,
                                contentDescription = stringResource(R.string.threads_overview_title),
                            )
                        }
                    }
                    if (searchTarget != null) {
                        IconButton(onClick = { searchOpen = true }) {
                            Icon(
                                Icons.Outlined.Search,
                                contentDescription = stringResource(R.string.search_messages),
                            )
                        }
                    }
                    // XEP-0492 bell: parent conversations only — a
                    // thread screen shares its parent's setting.
                    if (!viewModel.isThread) {
                        IconButton(onClick = { notifySheetOpen = true }) {
                            Icon(
                                if (notifyMode == WaddleNotifyMode.NEVER) {
                                    Icons.Outlined.NotificationsOff
                                } else {
                                    Icons.Outlined.Notifications
                                },
                                contentDescription = stringResource(R.string.notify_settings_title),
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
            aboveTimeline()
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
                authorPresence = authorPresence,
                trustedMediaOrigin = trustedMediaOrigin,
            )
            TypingIndicator(names = typing)
            val slashCommands = slashCommandHost
                ?.let { host -> host.controller.commands.collectAsStateWithLifecycle().value }
                .orEmpty()
            MessageComposer(
                onSend = viewModel::send,
                mentionCandidates = mentionCandidates,
                onDraftChanged = viewModel::onDraftChanged,
                editing = composerMode as? ComposerMode.Editing,
                onCancelEdit = viewModel::cancelEdit,
                replying = composerMode as? ComposerMode.Replying,
                onCancelReply = viewModel::cancelReply,
                onAttach = { attachmentPicker.launch("*/*") },
                onGif = { gifPickerOpen = true },
                onSticker = { stickerPickerOpen = true },
                uploadState = uploadState,
                onClearUpload = viewModel::clearUploadState,
                slashCommands = slashCommands,
                inMuc = viewModel.isGroupchat,
                onSlashArmed = { slashCommandHost?.controller?.ensureDiscovered() },
                slashDispatch = slashCommandHost?.let { host ->
                    slashDispatchOf(host) { body -> viewModel.send(body) }
                },
            )
        }
    }

    if (slashCommandHost != null) {
        SlashCommandSurfaces(
            host = slashCommandHost,
            snackbarHostState = snackbarHostState,
            sendPublicMessage = { body -> viewModel.send(body) },
        )
    }

    sheetTarget?.let { item ->
        MessageActionSheet(
            item = item,
            actionable = viewModel.actionTargetIdOf(item) != null,
            canPin = state.canPin,
            isPinned = item.identityIds.any { it in state.pinnedIds },
            canModerate = state.canModerate,
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
            onModerate = { moderateTarget = item },
        )
    }

    moderateTarget?.let { item ->
        AlertDialog(
            onDismissRequest = { moderateTarget = null },
            title = { Text(text = stringResource(R.string.moderate_confirm_title)) },
            text = { Text(text = stringResource(R.string.moderate_confirm_body)) },
            confirmButton = {
                TextButton(onClick = {
                    viewModel.moderate(item)
                    moderateTarget = null
                }) {
                    Text(
                        text = stringResource(R.string.action_moderate_message),
                        color = MaterialTheme.colorScheme.error,
                    )
                }
            },
            dismissButton = {
                TextButton(onClick = { moderateTarget = null }) {
                    Text(text = stringResource(R.string.action_cancel))
                }
            },
        )
    }

    if (notifySheetOpen) {
        NotifySettingsSheet(
            currentMode = notifyMode,
            onDismiss = { notifySheetOpen = false },
            onSelect = viewModel::setNotificationMode,
        )
    }

    if (gifPickerOpen) {
        // Graph read stays inside the open branch: hosts without a
        // provided graph (plain scaffold tests) never open the picker.
        val graph = LocalAppGraph.current
        GifPickerSheet(
            gateway = graph.gifSearchGateway,
            onSelect = { url ->
                gifPickerOpen = false
                viewModel.sendGif(url)
            },
            onDismiss = { gifPickerOpen = false },
        )
    }

    if (stickerPickerOpen) {
        // Graph read stays inside the open branch: hosts without a
        // provided graph (plain scaffold tests) never open the picker.
        val graph = LocalAppGraph.current
        val packs by graph.sessionManager.stickerPackStore.packs.collectAsStateWithLifecycle()
        LaunchedEffect(graph) { graph.sessionManager.loadStickerPacks() }
        // VM-scoped (not composition-scoped): a removal keeps running —
        // and reconciles the store — even when the sheet closes or the
        // screen rotates right after the confirm.
        val stickerSession by graph.currentSession.collectAsStateWithLifecycle()
        // Account-scoped key (ProfileScreen precedent): the activity
        // retains VM stores across logout/login, and account A's sticky
        // phase or in-flight pipeline must not serve account B.
        val packsViewModel: StickerPacksViewModel = viewModel(
            key = "$STICKER_PACKS_VIEW_MODEL_KEY:${stickerSession?.jid.orEmpty()}",
            factory = StickerPacksViewModel.factory(graph),
        )
        // Keyed on the VM, not the value (a value-keyed effect would
        // restart on consume and cancel showSnackbar mid-suspend), and
        // lifecycle-gated so a failure landing while STOPPED stays
        // sticky instead of being shown invisibly and consumed.
        // Consume AFTER showing, compare-and-set against the shown
        // value — a rotation mid-display re-shows once, and a distinct
        // failure landing mid-show survives.
        val stickerLifecycle = LocalLifecycleOwner.current
        LaunchedEffect(packsViewModel, stickerLifecycle) {
            stickerLifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) {
                packsViewModel.removeFailure.filterNotNull().collect { shown ->
                    snackbarHostState.showSnackbar(actionFailedText)
                    packsViewModel.consumeRemoveFailure(shown)
                }
            }
        }
        StickerPickerSheet(
            packs = packs,
            onSelect = { item, pack ->
                stickerPickerOpen = false
                viewModel.sendSticker(item, pack.id)
            },
            onCreatePack = {
                stickerPickerOpen = false
                createStickerPackOpen = true
            },
            onRemovePack = { pack -> packsViewModel.removePack(pack.id) },
            onDismiss = { stickerPickerOpen = false },
        )
    }

    if (createStickerPackOpen) {
        val graph = LocalAppGraph.current
        val stickerSession by graph.currentSession.collectAsStateWithLifecycle()
        // Same instance as the picker branch (shared account-scoped
        // key): the create pipeline lives in the VM and survives sheet
        // recreation.
        val packsViewModel: StickerPacksViewModel = viewModel(
            key = "$STICKER_PACKS_VIEW_MODEL_KEY:${stickerSession?.jid.orEmpty()}",
            factory = StickerPacksViewModel.factory(graph),
        )
        val createPhase by packsViewModel.createPhase.collectAsStateWithLifecycle()
        CreateStickerPackSheet(
            phase = createPhase,
            onCreate = packsViewModel::createPack,
            onCreated = {
                packsViewModel.consumeCreateSuccess()
                createStickerPackOpen = false
                // Back to the picker: the store already holds the new pack.
                stickerPickerOpen = true
            },
            onDismiss = {
                // A terminal Failed phase is last attempt's news; a
                // reopened sheet must start fresh.
                packsViewModel.resetCreatePhase()
                createStickerPackOpen = false
            },
        )
    }

    searchTarget?.takeIf { searchOpen }?.let { target ->
        // Graph read stays inside the open branch: hosts without a
        // provided graph (plain scaffold tests) never search.
        val graph = LocalAppGraph.current
        val searchViewModel: MessageSearchViewModel = viewModel(
            key = "search:${target.conversationJid}",
            factory = MessageSearchViewModel.factory(graph, target),
        )
        MessageSearchSheet(
            viewModel = searchViewModel,
            onDismiss = {
                searchOpen = false
                // Reopen starts fresh (web parity: closing resets), and
                // the ticket bump drops any in-flight response.
                searchViewModel.clear()
            },
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

/** Shared VM key: the picker and create sheets address ONE instance. */
private const val STICKER_PACKS_VIEW_MODEL_KEY = "sticker-packs"

/** Semantics tags for the DM call entry points, shared with tests. */
object ConversationCallTestTags {
    const val AUDIO_CALL_BUTTON = "conversation-call-audio"
    const val VIDEO_CALL_BUTTON = "conversation-call-video"
}

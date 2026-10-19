package social.waddle.android.feature.conversation

import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.automirrored.outlined.Chat
import androidx.compose.material.icons.outlined.Call
import androidx.compose.material.icons.outlined.MoreVert
import androidx.compose.material.icons.outlined.Notifications
import androidx.compose.material.icons.outlined.NotificationsOff
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material.icons.outlined.Videocam
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.AnnotatedString
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
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.WaddleTopBar
import social.waddle.client.ffi.WaddleNotifyMode

/**
 * Shared conversation scaffold (channel + DM + thread): top bar,
 * timeline, composer (on top of the timeline in the social layout). Marks the conversation active (unread clearing)
 * while resumed. [onOpenThread] is null on thread screens — threads do
 * not nest, so reply cards, opening a thread from a post and the
 * overview affordance hide. A message's avatar or author name opens
 * their profile; [onOpenDm] adds its "Message" action.
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
    /** Opens a DM from a message author's profile; `null` hides "Message". */
    onOpenDm: ((peerJid: String, name: String) -> Unit)? = null,
    /** Own bare JID for the self-mention row highlight (XEP-0372). */
    selfBareJid: String? = null,
    /** Presence line under the title (XEP-0319 idle, DM screens). */
    subtitle: String? = null,
    /** Origin whose cached XEP-0363 preview images may load. */
    trustedMediaOrigin: String? = null,
    /** Host icons before search (call controls, members). Keep it to one or two. */
    extraTopBarActions: @Composable () -> Unit = {},
    /** Host entries for the overflow menu; call `dismiss` before acting. */
    overflowItems: @Composable (dismiss: () -> Unit) -> Unit = {},
    /** Test tag for the overflow button. */
    overflowTestTag: String = ConversationTopBarTestTags.OVERFLOW,
    /** DM-only call entry points; `null` (channels, threads) hides them. */
    onStartCall: ((video: Boolean) -> Unit)? = null,
    /** Host slot rendered above the timeline (channel call banner). */
    aboveTimeline: @Composable () -> Unit = {},
    /** Slash-command wiring; `null` (scaffold tests) hides the feature. */
    slashCommandHost: SlashCommandHost? = null,
    /** Room, DM or thread: the empty state and placeholder. `null` infers it from [viewModel]. */
    kind: ConversationKind? = null,
    /** A face before the title (DM peer with presence). */
    titleLeading: (@Composable () -> Unit)? = null,
    /** Replaces [subtitle] under the title (room headcount and live huddle). */
    titleSubline: (@Composable () -> Unit)? = null,
) {
    val clock = rememberConversationClock()
    CompositionLocalProvider(LocalConversationClock provides clock) {
        ConversationScaffold(
            title = title,
            viewModel = viewModel,
            onBack = onBack,
            onOpenThread = onOpenThread,
            searchTarget = searchTarget,
            onOpenDm = onOpenDm,
            selfBareJid = selfBareJid,
            trustedMediaOrigin = trustedMediaOrigin,
            chrome = ConversationChromeSlots(
                titleLeading = titleLeading,
                titleSubline = titleSubline ?: subtitle?.let { text -> { DmHeaderSubline(text) } },
                extraTopBarActions = extraTopBarActions,
                overflowItems = overflowItems,
                overflowTestTag = overflowTestTag,
                onStartCall = onStartCall,
                aboveTimeline = aboveTimeline,
            ),
            slashCommandHost = slashCommandHost,
            kind = kind ?: when {
                viewModel.isThread -> ConversationKind.Thread
                viewModel.isGroupchat -> ConversationKind.Room(title)
                else -> ConversationKind.Dm(title)
            },
        )
    }
}

/** The top-bar and above-timeline slots a host fills. */
internal class ConversationChromeSlots(
    val titleLeading: (@Composable () -> Unit)?,
    val titleSubline: (@Composable () -> Unit)?,
    val extraTopBarActions: @Composable () -> Unit,
    val overflowItems: @Composable (dismiss: () -> Unit) -> Unit,
    val overflowTestTag: String,
    val onStartCall: ((video: Boolean) -> Unit)?,
    val aboveTimeline: @Composable () -> Unit,
)

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ConversationScaffold(
    title: String,
    viewModel: ConversationViewModel,
    onBack: () -> Unit,
    onOpenThread: ((threadId: String) -> Unit)?,
    searchTarget: MessageSearchTarget?,
    onOpenDm: ((peerJid: String, name: String) -> Unit)?,
    selfBareJid: String?,
    trustedMediaOrigin: String?,
    chrome: ConversationChromeSlots,
    slashCommandHost: SlashCommandHost?,
    kind: ConversationKind,
) {
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    val typing by viewModel.typing.collectAsStateWithLifecycle()
    val mentionCandidates by viewModel.mentionCandidates.collectAsStateWithLifecycle()
    val mentionDirectoryHits by viewModel.mentionDirectoryHits.collectAsStateWithLifecycle()
    val composerMode by viewModel.composerMode.collectAsStateWithLifecycle()
    val uploadState by viewModel.uploadState.collectAsStateWithLifecycle()
    val clipboard = LocalClipboardManager.current
    val attachmentPicker = rememberLauncherForActivityResult(
        ActivityResultContracts.GetContent(),
    ) { uri -> uri?.let(viewModel::sendAttachment) }
    val authorPresence by viewModel.authorPresence.collectAsStateWithLifecycle()
    val declaredBotJids by viewModel.declaredBotJids.collectAsStateWithLifecycle()
    var sheetTarget by remember { mutableStateOf<TimelineItem?>(null) }
    var moderateTarget by remember { mutableStateOf<TimelineItem?>(null) }
    var profileTarget by remember { mutableStateOf<MessageAuthor?>(null) }
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
            WaddleTopBar(
                title = {
                    ConversationTitle(title = title, leading = chrome.titleLeading, subline = chrome.titleSubline)
                },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(
                            Icons.AutoMirrored.Outlined.ArrowBack,
                            contentDescription = stringResource(R.string.action_back),
                        )
                    }
                },
                actions = {
                    chrome.extraTopBarActions()
                    chrome.onStartCall?.let { CallMenuAction(it) }
                    if (searchTarget != null) {
                        IconButton(onClick = { searchOpen = true }) {
                            Icon(Icons.Outlined.Search, contentDescription = stringResource(R.string.search_messages))
                        }
                    }
                    var overflowOpen by remember { mutableStateOf(false) }
                    val dismiss = { overflowOpen = false }
                    Box {
                        IconButton(
                            onClick = { overflowOpen = true },
                            modifier = Modifier.testTag(chrome.overflowTestTag),
                        ) {
                            Icon(
                                Icons.Outlined.MoreVert,
                                contentDescription = stringResource(R.string.conversation_more),
                            )
                        }
                        DropdownMenu(expanded = overflowOpen, onDismissRequest = dismiss) {
                            if (onOpenThread != null && state.threads.isNotEmpty()) {
                                DropdownMenuItem(
                                    text = { Text(text = stringResource(R.string.threads_overview_title)) },
                                    leadingIcon = { Icon(Icons.AutoMirrored.Outlined.Chat, contentDescription = null) },
                                    onClick = {
                                        dismiss()
                                        threadsOverviewOpen = true
                                    },
                                )
                            }
                            // XEP-0492: parent conversations only — a
                            // thread shares its parent's setting.
                            if (!viewModel.isThread) {
                                DropdownMenuItem(
                                    text = { Text(text = stringResource(R.string.notify_settings_title)) },
                                    leadingIcon = {
                                        Icon(
                                            if (notifyMode == WaddleNotifyMode.NEVER) {
                                                Icons.Outlined.NotificationsOff
                                            } else {
                                                Icons.Outlined.Notifications
                                            },
                                            contentDescription = null,
                                        )
                                    },
                                    onClick = {
                                        dismiss()
                                        notifySheetOpen = true
                                    },
                                )
                            }
                            chrome.overflowItems(dismiss)
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
            chrome.aboveTimeline()
            val rowActions = remember(viewModel, onOpenThread) {
                MessageRowActions(
                    onLongPress = { item -> sheetTarget = item },
                    onToggleReaction = viewModel::toggleReaction,
                    onOpenThread = onOpenThread?.let { open -> { item -> open(viewModel.threadIdFor(item)) } },
                    onRetry = viewModel::retry,
                    onOpenProfile = { author -> profileTarget = author },
                )
            }
            val rowContext = remember(selfBareJid, authorPresence, declaredBotJids, trustedMediaOrigin) {
                MessageRowContext(
                    selfBareJid = selfBareJid,
                    authorPresence = authorPresence,
                    declaredBotJids = declaredBotJids,
                    trustedMediaOrigin = trustedMediaOrigin,
                )
            }
            val timeline: @Composable ColumnScope.() -> Unit = {
                TimelineList(
                    entries = state.entries,
                    isLoadingOlder = state.isLoadingOlder,
                    reachedHistoryStart = state.reachedHistoryStart,
                    onTopReached = viewModel::loadOlder,
                    actions = rowActions,
                    context = rowContext,
                    modifier = Modifier.weight(1f),
                    onAtNewestEdgeChanged = viewModel::onAtNewestEdgeChanged,
                    loadingFirstPage = state.awaitingFirstPage,
                    emptyState = { TimelineEmpty(kind) },
                    newestAtTop = state.newestFirst,
                )
            }
            val slashCommands = slashCommandHost
                ?.let { host -> host.controller.commands.collectAsStateWithLifecycle().value }
                .orEmpty()
            val composer: @Composable () -> Unit = {
                MessageComposer(
                    onSend = viewModel::send,
                    mentionCandidates = mentionCandidates,
                    mentionDirectoryHits = mentionDirectoryHits,
                    onMentionQuery = viewModel::onMentionQueryChanged,
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
                    placeholder = composerPlaceholderOf(kind),
                    onImage = viewModel::sendAttachment,
                    pinnedTop = state.newestFirst,
                )
            }
            // Social (web "Newest at top"): the composer and who is typing
            // lead, the newest posts follow. Chat: they close the column.
            if (state.newestFirst) {
                composer()
                TypingIndicator(names = typing, avatarJidOf = viewModel::typingAuthorJid)
                timeline()
            } else {
                timeline()
                TypingIndicator(names = typing, avatarJidOf = viewModel::typingAuthorJid)
                composer()
            }
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

    profileTarget?.let { author ->
        val peer = author.jid?.takeIf { canMessage(it, selfBareJid, viewModel.dmPeerJid) }
        MessageAuthorProfileSheet(
            author = author,
            occupant = author.nick?.let { authorPresence[it] },
            onDismiss = { profileTarget = null },
            onMessage = if (peer != null && onOpenDm != null) {
                {
                    profileTarget = null
                    onOpenDm(peer, author.name)
                }
            } else {
                null
            },
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
                        color = Waddle.colors.dangerText,
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
        ThreadsOverviewSheet(
            threads = state.threads,
            selfBareJid = selfBareJid,
            onOpen = { threadId ->
                threadsOverviewOpen = false
                onOpenThread(threadId)
            },
            onDismiss = { threadsOverviewOpen = false },
        )
    }
}

/** Shared VM key: the picker and create sheets address ONE instance. */
private const val STICKER_PACKS_VIEW_MODEL_KEY = "sticker-packs"

/** One call icon; voice or video from its menu. */
@Composable
private fun CallMenuAction(onStartCall: (video: Boolean) -> Unit) {
    var open by remember { mutableStateOf(false) }
    Box {
        IconButton(onClick = { open = true }) {
            Icon(Icons.Outlined.Call, contentDescription = stringResource(R.string.call_start))
        }
        DropdownMenu(expanded = open, onDismissRequest = { open = false }) {
            DropdownMenuItem(
                text = { Text(text = stringResource(R.string.call_start_audio)) },
                leadingIcon = { Icon(Icons.Outlined.Call, contentDescription = null) },
                onClick = {
                    open = false
                    onStartCall(false)
                },
                modifier = Modifier.testTag(ConversationCallTestTags.AUDIO_CALL_BUTTON),
            )
            DropdownMenuItem(
                text = { Text(text = stringResource(R.string.call_start_video)) },
                leadingIcon = { Icon(Icons.Outlined.Videocam, contentDescription = null) },
                onClick = {
                    open = false
                    onStartCall(true)
                },
                modifier = Modifier.testTag(ConversationCallTestTags.VIDEO_CALL_BUTTON),
            )
        }
    }
}

/** Semantics tags for the conversation top bar. */
object ConversationTopBarTestTags {
    const val OVERFLOW = "conversation-overflow"
}

/** Semantics tags for the DM call entry points, shared with tests. */
object ConversationCallTestTags {
    const val AUDIO_CALL_BUTTON = "conversation-call-audio"
    const val VIDEO_CALL_BUTTON = "conversation-call-video"
}

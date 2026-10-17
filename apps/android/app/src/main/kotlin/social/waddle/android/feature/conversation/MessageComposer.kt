package social.waddle.android.feature.conversation

import android.net.Uri
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.content.MediaType
import androidx.compose.foundation.content.consume
import androidx.compose.foundation.content.contentReceiver
import androidx.compose.foundation.content.hasMediaType
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.text.input.TextFieldLineLimits
import androidx.compose.foundation.text.input.TextFieldState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.Send
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.AlternateEmail
import androidx.compose.material.icons.outlined.AttachFile
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.CloudUpload
import androidx.compose.material.icons.outlined.Code
import androidx.compose.material.icons.outlined.Edit
import androidx.compose.material.icons.outlined.EmojiEmotions
import androidx.compose.material.icons.outlined.FormatBold
import androidx.compose.material.icons.outlined.FormatItalic
import androidx.compose.material.icons.outlined.FormatQuote
import androidx.compose.material.icons.outlined.FormatStrikethrough
import androidx.compose.material.icons.outlined.Gif
import androidx.compose.material.icons.outlined.Terminal
import androidx.compose.material.icons.outlined.TextFields
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.IconButtonDefaults
import androidx.compose.material3.MenuDefaults
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.coerceIn
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch
import social.waddle.android.R
import social.waddle.android.client.ExtensionCommand
import social.waddle.android.client.MentionCandidate
import social.waddle.android.client.MentionRef
import social.waddle.android.theme.Waddle
import social.waddle.android.theme.WaddleTokens
import social.waddle.android.ui.huddle.HuddleMotion

/**
 * The composer card (web/iOS parity): reply/edit/upload context, an
 * optional formatting bar, the text, then + menu · Aa · @ · / and a
 * filled send button. Send clears the draft optimistically.
 * Group chats pass [mentionCandidates]: an active `@token` under the
 * cursor opens [MentionPopover], and accepted mentions travel with the
 * send as XEP-0372 refs.
 *
 * A draft that is entirely a `/prefix …` slash trigger (and
 * [slashDispatch] is wired) is intercepted at the send button: a
 * resolved command dispatches through [slashDispatch] — the draft
 * clears only when the dispatch reports success, so a failure leaves
 * the slash draft armed for a retry — and an unresolved prefix blocks
 * the send with an inline hint (web `slashSubmitBlocked` parity).
 * Two web-parity escapes send the draft as plain text instead: a bare
 * `/` with no prefix, and a prefix whose popover the user dismissed.
 */
@Composable
fun MessageComposer(
    onSend: (body: String, mentions: List<MentionRef>) -> Unit,
    modifier: Modifier = Modifier,
    onDraftChanged: () -> Unit = {},
    editing: ComposerMode.Editing? = null,
    onCancelEdit: () -> Unit = {},
    replying: ComposerMode.Replying? = null,
    onCancelReply: () -> Unit = {},
    onAttach: (() -> Unit)? = null,
    /** Opens the GIF picker sheet; `null` hides the affordance. */
    onGif: (() -> Unit)? = null,
    /** Opens the sticker picker sheet; `null` hides the affordance. */
    onSticker: (() -> Unit)? = null,
    uploadState: UploadState = UploadState.Idle,
    onClearUpload: () -> Unit = {},
    mentionCandidates: List<MentionCandidate> = emptyList(),
    /** Discovered slash commands; empty until discovery answers. */
    slashCommands: List<ExtensionCommand> = emptyList(),
    /** Channel-scoped commands only match inside MUC rooms. */
    inMuc: Boolean = false,
    /** Fires when the draft becomes a slash trigger (start discovery). */
    onSlashArmed: () -> Unit = {},
    /** Runs a resolved invocation; `null` disables slash handling. */
    slashDispatch: (suspend (SlashInvocation) -> Boolean)? = null,
    /** "Message #general", "Message alice"; `null` reads "Message". */
    placeholder: String? = null,
    /**
     * An image pasted, inserted from the keyboard or dropped on the text
     * field; `null` leaves such content alone.
     */
    onImage: ((Uri) -> Unit)? = null,
    /**
     * Social layout: the composer sits on top of the timeline, so its
     * hairline moves below it and the popovers open underneath.
     */
    pinnedTop: Boolean = false,
) {
    var draft by rememberSaveable(stateSaver = TextFieldValue.Saver) {
        mutableStateOf(TextFieldValue(""))
    }
    // Mention spans survive edits via the tracker, not saveable state: a
    // process death keeps the draft TEXT but demotes its mentions to
    // plain `@Nick` text (a resent label without its span is just text —
    // never a mislabelled reference).
    val mentionTracker = remember { MentionSpanTracker() }
    // Unresolved `/xyz` prefix the send button refused; cleared on the
    // next text change so the hint never outlives the draft it blocked.
    var slashBlockedPrefix by remember { mutableStateOf<String?>(null) }

    fun updateDraft(value: TextFieldValue) {
        if (value.text != draft.text) slashBlockedPrefix = null
        mentionTracker.onTextChanged(value.text)
        draft = value
    }
    // Track the last-seen edit CONTENT across recreation: the reset must
    // fire only on actual transitions — an unconditional
    // LaunchedEffect(editing) also runs on every initial composition
    // (rotation, process restore) and would wipe the restored draft.
    // Keyed by targetId AND body so a failed edit's restore (same
    // target, the user's attempted text) repopulates even when the
    // intermediate Normal state was never composed.
    var lastEditKey by rememberSaveable { mutableStateOf<String?>(null) }
    // The in-progress message stashed when an edit begins, restored when
    // the edit ends (sent or cancelled) — entering edit mode must not
    // destroy what the user was typing.
    var stashedDraft by rememberSaveable { mutableStateOf("") }

    val editKey = editing?.let { "${it.attempt}:${it.targetId} ${it.originalBody}" }
    LaunchedEffect(editKey) {
        if (editKey == lastEditKey) return@LaunchedEffect
        if (editing != null) {
            if (lastEditKey == null) stashedDraft = draft.text
            updateDraft(TextFieldValue(editing.originalBody, TextRange(editing.originalBody.length)))
        } else {
            // Editing ended (live send/cancel), or the composition is
            // fresh with a stale edit key — a process death mid-edit or
            // a navigation return. In every case the pre-edit stash is
            // what belongs in the field: the process-death edit text
            // itself is DISCARDED, because with the edit context gone a
            // send would post the old body as a brand-new message (the
            // wrong wire shape beats losing an in-progress edit).
            updateDraft(TextFieldValue(stashedDraft, TextRange(stashedDraft.length)))
            stashedDraft = ""
        }
        lastEditKey = editKey
    }

    // Mention autocomplete arms on a collapsed cursor inside an `@token`
    // (never in edit mode — XEP-0308 corrections drop mention refs, so
    // offering the popover there would lie about the wire shape).
    val mentionToken = if (editing == null && mentionCandidates.isNotEmpty() && draft.selection.collapsed) {
        activeMentionToken(draft.text, draft.selection.start)
    } else {
        null
    }
    val mentionResults = mentionToken?.let { filterMentionCandidates(mentionCandidates, it.query) }.orEmpty()

    // Slash-command autocomplete arms when the whole draft is a
    // `/prefix` trigger (never in edit mode — a XEP-0308 correction of
    // `/poll` is a text fix, not a command; cursor must be collapsed
    // like mentions).
    val slashTrigger = if (slashDispatch != null && editing == null && draft.selection.collapsed) {
        parseSlashTrigger(draft.text)
    } else {
        null
    }
    // Per-prefix dismissal memory: closing the popover hides it until
    // the typed prefix changes; disarming forgets the dismissal.
    var dismissedSlashPrefix by remember { mutableStateOf<String?>(null) }
    LaunchedEffect(slashTrigger != null) {
        if (slashTrigger != null) onSlashArmed() else dismissedSlashPrefix = null
    }
    val slashCandidates = slashTrigger
        ?.takeIf { it.trailing.isEmpty() && it.prefix != dismissedSlashPrefix }
        ?.let { filterSlashCandidates(it.prefix, slashCommands, inMuc) }
        .orEmpty()
    var slashDispatching by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()

    var formattingOpen by rememberSaveable { mutableStateOf(false) }
    val haptics = LocalHapticFeedback.current

    fun submit() {
        val trigger = if (slashDispatch != null && editing == null) {
            parseSlashTrigger(draft.text)?.takeIf { candidate ->
                shouldInterceptSlashSend(candidate, dismissedSlashPrefix)
            }
        } else {
            null
        }
        if (trigger == null) {
            haptics.performHapticFeedback(HapticFeedbackType.Confirm)
            onSend(draft.text, mentionTracker.mentionRefs())
            updateDraft(TextFieldValue(""))
            return
        }
        val command = resolveSlashCommand(trigger.prefix, slashCommands, inMuc)
        if (command == null || slashDispatch == null) {
            // Unresolved (or ambiguous) prefix: block the send instead of
            // posting a broken command as a message (web slashSubmitBlocked
            // parity).
            slashBlockedPrefix = "/${trigger.prefix}"
            return
        }
        val invocation = buildSlashInvocation(command, trigger.trailing)
        slashDispatching = true
        scope.launch {
            val handled = try {
                slashDispatch(invocation)
            } finally {
                slashDispatching = false
            }
            // Only a successful handoff clears the draft — failure leaves
            // slash mode armed.
            if (handled) updateDraft(TextFieldValue(""))
        }
    }

    fun insert(edit: (TextFieldValue) -> TextFieldValue) {
        updateDraft(edit(draft))
        onDraftChanged()
    }

    var focused by remember { mutableStateOf(false) }
    val colors = Waddle.colors
    // Above the card, or under it when the composer is pinned on top.
    val popovers: @Composable () -> Unit = {
        if (slashCandidates.isNotEmpty()) {
            SlashCommandPopover(
                candidates = slashCandidates,
                onSelect = { command ->
                    val completed = "/${command.composerPrefix.orEmpty()} "
                    updateDraft(TextFieldValue(completed, TextRange(completed.length)))
                    onDraftChanged()
                },
                onDismiss = { dismissedSlashPrefix = slashTrigger?.prefix },
            )
        }
        if (mentionToken != null && mentionResults.isNotEmpty()) {
            MentionPopover(
                candidates = mentionResults,
                onSelect = { candidate ->
                    mentionTracker.insertMention(draft.text, mentionToken, candidate)?.let { insertion ->
                        // The tracker already synced to the inserted text;
                        // going through updateDraft would re-diff the change.
                        draft = TextFieldValue(insertion.text, TextRange(insertion.cursor))
                        // Accepting a candidate mutates the draft like a
                        // keystroke — the XEP-0085 composing state refreshes.
                        onDraftChanged()
                    }
                },
            )
        }
    }
    Column(
        modifier = modifier
            .fillMaxWidth()
            .background(colors.bg)
            .drawBehind {
                // The hairline parts the composer from the timeline.
                val y = if (pinnedTop) size.height else 0f
                drawLine(colors.borderSoft, Offset(0f, y), Offset(size.width, y), 1.dp.toPx())
            }
            .then(if (pinnedTop) Modifier else Modifier.navigationBarsPadding())
            .padding(horizontal = Waddle.space.sm, vertical = Waddle.space.sm),
    ) {
        if (!pinnedTop) popovers()
        ComposerCard(focused = focused) {
            slashBlockedPrefix?.let { blocked ->
                // Dismissing the hint is the mobile Esc: it disarms interception
                // for this prefix so the next send posts the draft as plain text.
                // Without it, a prefix that matches no command (no popover to
                // dismiss) would be unsendable.
                ComposerBanner(
                    title = stringResource(R.string.composer_slash_unknown, blocked),
                    tone = BannerTone.DANGER,
                    cancelContentDescription = stringResource(R.string.composer_slash_send_as_text),
                    onCancel = {
                        dismissedSlashPrefix = blocked.removePrefix("/")
                        slashBlockedPrefix = null
                    },
                    modifier = Modifier.testTag(SlashCommandTestTags.BLOCKED_HINT),
                    cancelModifier = Modifier.testTag(SlashCommandTestTags.BLOCKED_HINT_DISMISS),
                )
            }
            ComposerContext(
                editing = editing,
                onCancelEdit = onCancelEdit,
                replying = replying,
                onCancelReply = onCancelReply,
                uploadState = uploadState,
                onClearUpload = onClearUpload,
            )
            AnimatedVisibility(visible = formattingOpen) {
                FormattingBar(onFormat = { format -> insert { applyFormat(it, format) } })
            }
            ComposerTextField(
                value = draft,
                placeholder = placeholder ?: stringResource(R.string.composer_placeholder),
                // One upload at a time: content arriving meanwhile is left alone.
                onImage = onImage?.takeIf { uploadState != UploadState.Uploading },
                onFocusChanged = { focused = it },
                onValueChange = { value ->
                    if (value.text != draft.text) onDraftChanged()
                    updateDraft(value)
                },
            )
            ComposerActionRow(
                canAttach = onAttach != null && uploadState != UploadState.Uploading,
                onAttach = onAttach,
                onGif = onGif,
                onSticker = onSticker,
                formattingOpen = formattingOpen,
                onToggleFormatting = { formattingOpen = !formattingOpen },
                onMention = if (mentionCandidates.isNotEmpty()) ({ insert { insertTrigger(it, "@") } }) else null,
                onSlash = if (slashDispatch != null) ({ insert { insertTrigger(it, "/") } }) else null,
                canSend = draft.text.isNotBlank() && !slashDispatching,
                onSend = ::submit,
            )
        }
        if (pinnedTop) popovers()
    }
}

/**
 * The composer card (web `.composer`): radius 14 on the card colour with
 * a hairline border; focus turns the border teal with a 1dp teal ring.
 */
@Composable
private fun ComposerCard(focused: Boolean, content: @Composable ColumnScope.() -> Unit) {
    val colors = Waddle.colors
    val shape = RoundedCornerShape(COMPOSER_RADIUS)
    val border by animateColorAsState(
        if (focused) colors.action else colors.border,
        animationSpec = HuddleMotion.fast(),
        label = "composer-border",
    )
    val ring by animateColorAsState(
        if (focused) colors.action.copy(alpha = FOCUS_RING_ALPHA) else Color.Transparent,
        animationSpec = HuddleMotion.fast(),
        label = "composer-ring",
    )
    Surface(
        shape = shape,
        color = colors.surface,
        contentColor = colors.fg,
        border = BorderStroke(1.dp, border),
        modifier = Modifier
            .fillMaxWidth()
            .drawBehind {
                // The focus ring sits just outside the border, like CSS outline.
                val inset = -FOCUS_RING.toPx() / 2f
                drawRoundRect(
                    color = ring,
                    topLeft = Offset(inset, inset),
                    size = Size(size.width - inset * 2, size.height - inset * 2),
                    cornerRadius = CornerRadius(COMPOSER_RADIUS.toPx() - inset),
                    style = Stroke(FOCUS_RING.toPx()),
                )
            },
    ) {
        Column(content = content)
    }
}

/**
 * The draft's text field. It runs on a [TextFieldState], the only text
 * field that can receive content (pasted, keyboard-inserted or dropped
 * images), while the composer keeps its draft as a [TextFieldValue]:
 * the user's edits report through [onValueChange], and a [value] the
 * composer sets itself (send, mention, format, edit) is written back.
 */
@Composable
private fun ComposerTextField(
    value: TextFieldValue,
    placeholder: String,
    onImage: ((Uri) -> Unit)?,
    onFocusChanged: (Boolean) -> Unit,
    onValueChange: (TextFieldValue) -> Unit,
) {
    val state = remember { TextFieldState(value.text, value.selection) }
    // What both sides last agreed on: an echo of the user's own edit is
    // never written back over typing that has happened since.
    val synced = remember { SyncedDraft(value.text, value.selection) }
    val latestOnValueChange by rememberUpdatedState(onValueChange)
    SideEffect {
        if (!synced.matches(value.text, value.selection)) {
            synced.set(value.text, value.selection)
            state.edit {
                replace(0, length, value.text)
                selection = value.selection.coerceIn(0, length)
            }
        }
    }
    LaunchedEffect(state) {
        snapshotFlow { state.text.toString() to state.selection }
            .collect { (text, selection) ->
                if (!synced.matches(text, selection)) {
                    synced.set(text, selection)
                    latestOnValueChange(TextFieldValue(text, selection))
                }
            }
    }
    val colors = Waddle.colors
    val textStyle = Waddle.type.body.copy(color = colors.fg)
    BasicTextField(
        state = state,
        textStyle = textStyle,
        cursorBrush = SolidColor(colors.action),
        lineLimits = TextFieldLineLimits.MultiLine(maxHeightInLines = COMPOSER_MAX_LINES),
        keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.Sentences),
        modifier = Modifier
            .fillMaxWidth()
            .imageReceiver(onImage)
            .onFocusChanged { onFocusChanged(it.isFocused) }
            .padding(start = Waddle.space.md, end = Waddle.space.md, top = Waddle.space.sm),
        decorator = { inner ->
            Box {
                if (state.text.isEmpty()) {
                    Text(
                        text = placeholder,
                        style = textStyle,
                        color = colors.fgMuted,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
                inner()
            }
        },
    )
}

/** The text and selection the composer and its field last agreed on. */
private class SyncedDraft(private var text: String, private var selection: TextRange) {
    fun matches(text: String, selection: TextRange): Boolean = this.text == text && this.selection == selection

    fun set(text: String, selection: TextRange) {
        this.text = text
        this.selection = selection
    }
}

/**
 * Hands the first image of content pasted, inserted from the keyboard or
 * dropped here to [onImage]; the rest (text above all) goes on to the
 * field. `null` takes nothing.
 */
@OptIn(ExperimentalFoundationApi::class)
private fun Modifier.imageReceiver(onImage: ((Uri) -> Unit)?): Modifier = if (onImage == null) {
    this
} else {
    contentReceiver { content ->
        if (!content.hasMediaType(MediaType.Image)) return@contentReceiver content
        var taken = false
        content.consume { item ->
            val uri = item.uri
            if (taken || uri == null) {
                false
            } else {
                taken = true
                onImage(uri)
                true
            }
        }
    }
}

/** Reply / edit / upload state, inside the card above the text. */
@Composable
private fun ComposerContext(
    editing: ComposerMode.Editing?,
    onCancelEdit: () -> Unit,
    replying: ComposerMode.Replying?,
    onCancelReply: () -> Unit,
    uploadState: UploadState,
    onClearUpload: () -> Unit,
) {
    if (editing != null) {
        ComposerBanner(
            title = stringResource(R.string.composer_editing),
            quote = editing.originalBody,
            icon = Icons.Outlined.Edit,
            cancelContentDescription = stringResource(R.string.composer_cancel_edit),
            onCancel = onCancelEdit,
        )
    }
    if (replying != null && editing == null) {
        ComposerBanner(
            title = stringResource(R.string.composer_replying_to, replying.authorName),
            quote = replying.previewBody,
            cancelContentDescription = stringResource(R.string.composer_cancel_reply),
            onCancel = onCancelReply,
        )
    }
    when (uploadState) {
        UploadState.Idle -> Unit
        UploadState.Uploading -> ComposerBanner(
            title = stringResource(R.string.upload_in_progress),
            icon = Icons.Outlined.CloudUpload,
            cancelContentDescription = null,
            onCancel = null,
        )
        UploadState.TooLarge -> ComposerBanner(
            title = stringResource(R.string.upload_too_large),
            tone = BannerTone.DANGER,
            cancelContentDescription = stringResource(R.string.upload_dismiss),
            onCancel = onClearUpload,
        )
        UploadState.Failed -> ComposerBanner(
            title = stringResource(R.string.upload_failed),
            tone = BannerTone.DANGER,
            cancelContentDescription = stringResource(R.string.upload_dismiss),
            onCancel = onClearUpload,
        )
    }
}

/** + · Aa · @ · / … send; every control is a 48dp target. */
@Composable
private fun ComposerActionRow(
    canAttach: Boolean,
    onAttach: (() -> Unit)?,
    onGif: (() -> Unit)?,
    onSticker: (() -> Unit)?,
    formattingOpen: Boolean,
    onToggleFormatting: () -> Unit,
    onMention: (() -> Unit)?,
    onSlash: (() -> Unit)?,
    canSend: Boolean,
    onSend: () -> Unit,
) {
    var menuOpen by remember { mutableStateOf(false) }
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = Waddle.space.xxs),
    ) {
        if (onAttach != null || onGif != null || onSticker != null) {
            Box {
                PlusButton(open = menuOpen, onClick = { menuOpen = true })
                DropdownMenu(
                    expanded = menuOpen,
                    onDismissRequest = { menuOpen = false },
                    shape = RoundedCornerShape(Waddle.radius.panel),
                    containerColor = Waddle.colors.surface,
                    border = BorderStroke(1.dp, Waddle.colors.border),
                ) {
                    onAttach?.let { attach ->
                        ComposerMenuItem(Icons.Outlined.AttachFile, R.string.composer_attach, enabled = canAttach) {
                            menuOpen = false
                            attach()
                        }
                    }
                    onGif?.let { gif ->
                        ComposerMenuItem(Icons.Outlined.Gif, R.string.composer_gif) {
                            menuOpen = false
                            gif()
                        }
                    }
                    onSticker?.let { sticker ->
                        ComposerMenuItem(Icons.Outlined.EmojiEmotions, R.string.composer_sticker) {
                            menuOpen = false
                            sticker()
                        }
                    }
                }
            }
        }
        ComposerIconButton(
            icon = Icons.Outlined.TextFields,
            contentDescription = stringResource(R.string.composer_formatting),
            onClick = onToggleFormatting,
            selected = formattingOpen,
        )
        onMention?.let {
            ComposerIconButton(
                icon = Icons.Outlined.AlternateEmail,
                contentDescription = stringResource(R.string.composer_mention),
                onClick = it,
            )
        }
        onSlash?.let {
            ComposerIconButton(
                icon = Icons.Outlined.Terminal,
                contentDescription = stringResource(R.string.composer_command),
                onClick = it,
            )
        }
        Spacer(Modifier.weight(1f))
        SendButton(enabled = canSend, onClick = onSend)
    }
}

/** The "+" menu button: a 32dp muted circle whose plus turns 45° while the menu is open. */
@Composable
private fun PlusButton(open: Boolean, onClick: () -> Unit) {
    val colors = Waddle.colors
    val turn by animateFloatAsState(if (open) PLUS_TURN else 0f, HuddleMotion.fast(), label = "plus-turn")
    IconButton(
        onClick = onClick,
        colors = IconButtonDefaults.iconButtonColors(contentColor = colors.fgMuted),
    ) {
        Box(
            contentAlignment = Alignment.Center,
            modifier = Modifier
                .size(Waddle.control.sm)
                .background(colors.surface2, CircleShape),
        ) {
            Icon(
                Icons.Outlined.Add,
                contentDescription = stringResource(R.string.composer_more),
                modifier = Modifier
                    .size(COMPOSER_ICON)
                    .graphicsLayer { rotationZ = turn },
            )
        }
    }
}

@Composable
private fun ComposerMenuItem(
    icon: ImageVector,
    label: Int,
    enabled: Boolean = true,
    onClick: () -> Unit,
) {
    DropdownMenuItem(
        text = { Text(text = stringResource(label), style = Waddle.type.control) },
        leadingIcon = { Icon(icon, contentDescription = null) },
        enabled = enabled,
        onClick = onClick,
        colors = MenuDefaults.itemColors(
            textColor = Waddle.colors.fg,
            leadingIconColor = Waddle.colors.fgMuted,
        ),
    )
}

@Composable
private fun ComposerIconButton(
    icon: ImageVector,
    contentDescription: String,
    onClick: () -> Unit,
    selected: Boolean = false,
) {
    val colors = Waddle.colors
    IconButton(
        onClick = onClick,
        shape = RoundedCornerShape(Waddle.radius.control),
        colors = IconButtonDefaults.iconButtonColors(
            containerColor = if (selected) colors.action.copy(alpha = SELECTED_ALPHA) else Color.Transparent,
            contentColor = if (selected) colors.actionText else colors.fgMuted,
        ),
    ) {
        Icon(icon, contentDescription = contentDescription, modifier = Modifier.size(COMPOSER_ICON))
    }
}

/**
 * Send (web `.composer-send`): a radius-10 tile, muted until there is
 * something to send, then teal with dark ink.
 */
@Composable
private fun SendButton(enabled: Boolean, onClick: () -> Unit) {
    val colors = Waddle.colors
    val container by animateColorAsState(
        if (enabled) colors.action else colors.surface2,
        animationSpec = HuddleMotion.fast(),
        label = "send-container",
    )
    val content by animateColorAsState(
        if (enabled) colors.actionFg else colors.fgSoft,
        animationSpec = HuddleMotion.fast(),
        label = "send-content",
    )
    IconButton(
        onClick = onClick,
        enabled = enabled,
        shape = RoundedCornerShape(Waddle.radius.control),
        colors = IconButtonDefaults.iconButtonColors(
            containerColor = container,
            contentColor = content,
            disabledContainerColor = container,
            disabledContentColor = content,
        ),
    ) {
        Icon(
            Icons.AutoMirrored.Outlined.Send,
            contentDescription = stringResource(R.string.composer_send),
            modifier = Modifier.size(COMPOSER_ICON),
        )
    }
}

/** Markdown the composer converts to XEP-0394 at send (ComposerMarkdown). */
enum class ComposerFormat(val prefix: String, val suffix: String) {
    BOLD("**", "**"),
    ITALIC("*", "*"),
    STRIKE("~~", "~~"),
    CODE("`", "`"),
    CODE_BLOCK("```\n", "\n```"),
    QUOTE("> ", ""),
}

@Composable
private fun FormattingBar(onFormat: (ComposerFormat) -> Unit) {
    Row(
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.xxs),
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = Waddle.space.xs, vertical = Waddle.space.xxs),
    ) {
        FORMAT_ACTIONS.forEach { (format, icon, label) ->
            ComposerIconButton(icon = icon, contentDescription = stringResource(label), onClick = { onFormat(format) })
        }
    }
}

private val FORMAT_ACTIONS: List<Triple<ComposerFormat, ImageVector, Int>> = listOf(
    Triple(ComposerFormat.BOLD, Icons.Outlined.FormatBold, R.string.composer_format_bold),
    Triple(ComposerFormat.ITALIC, Icons.Outlined.FormatItalic, R.string.composer_format_italic),
    Triple(ComposerFormat.STRIKE, Icons.Outlined.FormatStrikethrough, R.string.composer_format_strike),
    Triple(ComposerFormat.CODE, Icons.Outlined.Code, R.string.composer_format_code),
    Triple(ComposerFormat.QUOTE, Icons.Outlined.FormatQuote, R.string.composer_format_quote),
)

/**
 * Wrap the selection in [format]'s markers (or insert an empty pair and
 * park the cursor between them). Quotes prefix the selection's first line.
 */
fun applyFormat(value: TextFieldValue, format: ComposerFormat): TextFieldValue {
    val text = value.text
    val start = value.selection.min
    val end = value.selection.max
    if (format == ComposerFormat.QUOTE) {
        val previousBreak = text.lastIndexOf('\n', (start - 1).coerceAtLeast(0))
        val lineStart = if (start == 0 || previousBreak < 0) 0 else previousBreak + 1
        val quoted = text.substring(0, lineStart) + format.prefix + text.substring(lineStart)
        return TextFieldValue(quoted, TextRange(start + format.prefix.length, end + format.prefix.length))
    }
    val wrapped = text.substring(0, start) + format.prefix +
        text.substring(start, end) + format.suffix + text.substring(end)
    val cursorStart = start + format.prefix.length
    return TextFieldValue(wrapped, TextRange(cursorStart, cursorStart + (end - start)))
}

/** Insert a trigger character at the cursor, spaced from a preceding word. */
fun insertTrigger(value: TextFieldValue, trigger: String): TextFieldValue {
    val text = value.text
    val at = value.selection.min
    val needsSpace = at > 0 && !text[at - 1].isWhitespace()
    val inserted = (if (needsSpace) " " else "") + trigger
    val next = text.substring(0, at) + inserted + text.substring(value.selection.max)
    return TextFieldValue(next, TextRange(at + inserted.length))
}

/** What a [ComposerBanner] is about, which decides its colour. */
private enum class BannerTone { ACTION, DANGER }

/**
 * One line of composer context (web reply banner): a 3dp bar on a muted
 * tint, the title, an optional italic quote, and a close button. Reply,
 * edit and upload are teal; a refused upload or command is danger.
 */
@Composable
private fun ComposerBanner(
    title: String,
    cancelContentDescription: String?,
    onCancel: (() -> Unit)?,
    modifier: Modifier = Modifier,
    quote: String? = null,
    icon: ImageVector? = null,
    tone: BannerTone = BannerTone.ACTION,
    cancelModifier: Modifier = Modifier,
) {
    val colors = Waddle.colors
    val accent = if (tone == BannerTone.DANGER) colors.dangerText else colors.action
    val ink = if (tone == BannerTone.DANGER) colors.dangerText else colors.fg
    val shape = RoundedCornerShape(Waddle.radius.control)
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = modifier
            .fillMaxWidth()
            .padding(start = Waddle.space.xs, end = Waddle.space.xs, top = Waddle.space.xs)
            .height(IntrinsicSize.Min)
            .clip(shape)
            .background(colors.surface2.copy(alpha = BANNER_FILL_ALPHA), shape),
    ) {
        Box(
            Modifier
                .width(BANNER_BAR)
                .fillMaxHeight()
                .background(accent),
        )
        icon?.let {
            Icon(
                it,
                contentDescription = null,
                tint = accent,
                modifier = Modifier
                    .padding(start = Waddle.space.sm)
                    .size(BANNER_ICON),
            )
        }
        Column(
            modifier = Modifier
                .weight(1f)
                .padding(start = Waddle.space.sm, top = Waddle.space.xs, bottom = Waddle.space.xs),
        ) {
            Text(
                text = title,
                style = MetaStyle.copy(fontWeight = FontWeight.SemiBold),
                color = ink,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            quote?.takeIf { it.isNotBlank() }?.let {
                Text(
                    text = it.lineSequence().first { line -> line.isNotBlank() },
                    style = MetaStyle,
                    fontStyle = FontStyle.Italic,
                    color = colors.fgMuted,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
        if (onCancel != null) {
            IconButton(onClick = onCancel, modifier = cancelModifier) {
                Icon(
                    Icons.Outlined.Close,
                    contentDescription = cancelContentDescription,
                    tint = colors.fgMuted,
                    modifier = Modifier.size(BANNER_ICON),
                )
            }
        }
    }
}

/** Web composer radius (14); between the control (10) and panel (12) radii on purpose. */
private val COMPOSER_RADIUS = WaddleTokens.Radius.xl
private val FOCUS_RING = 1.dp
private val COMPOSER_ICON = 20.dp
private val BANNER_BAR = 3.dp
private val BANNER_ICON = 16.dp
private const val COMPOSER_MAX_LINES = 6
private const val FOCUS_RING_ALPHA = 0.45f
private const val SELECTED_ALPHA = 0.12f
private const val BANNER_FILL_ALPHA = 0.7f
private const val PLUS_TURN = 45f

package social.waddle.android.feature.conversation

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.core.tween
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material.icons.automirrored.outlined.Reply
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.AlternateEmail
import androidx.compose.material.icons.outlined.AttachFile
import androidx.compose.material.icons.outlined.CloudUpload
import androidx.compose.material.icons.outlined.Code
import androidx.compose.material.icons.outlined.Edit
import androidx.compose.material.icons.outlined.FormatBold
import androidx.compose.material.icons.outlined.FormatItalic
import androidx.compose.material.icons.outlined.FormatQuote
import androidx.compose.material.icons.outlined.FormatStrikethrough
import androidx.compose.material.icons.outlined.Gif
import androidx.compose.material.icons.outlined.Mood
import androidx.compose.material.icons.outlined.Terminal
import androidx.compose.material.icons.outlined.TextFormat
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.IconButtonDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.TextRange
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

    Column(
        modifier = modifier
            .fillMaxWidth()
            .background(MaterialTheme.colorScheme.background)
            .navigationBarsPadding()
            .padding(start = Waddle.space.sm, end = Waddle.space.sm, top = Waddle.space.xxs, bottom = Waddle.space.xs),
    ) {
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
        slashBlockedPrefix?.let { blocked ->
            // Dismissing the hint is the mobile Esc: it disarms interception
            // for this prefix so the next send posts the draft as plain text.
            // Without it, a prefix that matches no command (no popover to
            // dismiss) would be unsendable.
            ComposerNotice(
                text = stringResource(R.string.composer_slash_unknown, blocked),
                icon = null,
                isError = true,
                cancelContentDescription = stringResource(R.string.composer_slash_send_as_text),
                onCancel = {
                    dismissedSlashPrefix = blocked.removePrefix("/")
                    slashBlockedPrefix = null
                },
                modifier = Modifier.testTag(SlashCommandTestTags.BLOCKED_HINT),
                cancelModifier = Modifier.testTag(SlashCommandTestTags.BLOCKED_HINT_DISMISS),
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
        ComposerCard {
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
    }
}

/** The rounded composer surface; its border takes the accent while focused. */
@Composable
private fun ComposerCard(content: @Composable ColumnScope.() -> Unit) {
    Surface(
        shape = RoundedCornerShape(COMPOSER_RADIUS),
        color = MaterialTheme.colorScheme.surfaceContainer,
        border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(content = content)
    }
}

@Composable
private fun ComposerTextField(value: TextFieldValue, onValueChange: (TextFieldValue) -> Unit) {
    val textStyle = MaterialTheme.typography.bodyLarge.copy(color = MaterialTheme.colorScheme.onSurface)
    BasicTextField(
        value = value,
        onValueChange = onValueChange,
        textStyle = textStyle,
        cursorBrush = SolidColor(MaterialTheme.colorScheme.primary),
        maxLines = COMPOSER_MAX_LINES,
        keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.Sentences),
        modifier = Modifier
            .fillMaxWidth()
            .padding(start = 14.dp, end = 14.dp, top = Waddle.space.sm, bottom = Waddle.space.xxs),
        decorationBox = { inner ->
            Box {
                if (value.text.isEmpty()) {
                    Text(
                        text = stringResource(R.string.composer_placeholder),
                        style = textStyle,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                inner()
            }
        },
    )
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
        ComposerNotice(
            text = stringResource(R.string.composer_editing),
            icon = Icons.Outlined.Edit,
            cancelContentDescription = stringResource(R.string.composer_cancel_edit),
            onCancel = onCancelEdit,
        )
    }
    if (replying != null && editing == null) {
        ComposerNotice(
            text = stringResource(R.string.composer_replying_to, replying.authorName),
            icon = Icons.AutoMirrored.Outlined.Reply,
            cancelContentDescription = stringResource(R.string.composer_cancel_reply),
            onCancel = onCancelReply,
        )
    }
    when (uploadState) {
        UploadState.Idle -> Unit
        UploadState.Uploading -> ComposerNotice(
            text = stringResource(R.string.upload_in_progress),
            icon = Icons.Outlined.CloudUpload,
            cancelContentDescription = null,
            onCancel = null,
        )
        UploadState.TooLarge -> ComposerNotice(
            text = stringResource(R.string.upload_too_large),
            icon = null,
            isError = true,
            cancelContentDescription = stringResource(R.string.upload_dismiss),
            onCancel = onClearUpload,
        )
        UploadState.Failed -> ComposerNotice(
            text = stringResource(R.string.upload_failed),
            icon = null,
            isError = true,
            cancelContentDescription = stringResource(R.string.upload_dismiss),
            onCancel = onClearUpload,
        )
    }
}

/** + menu · Aa · @ · / … send. */
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
            .padding(start = Waddle.space.xxs, end = Waddle.space.xs, bottom = Waddle.space.xs),
    ) {
        if (onAttach != null || onGif != null || onSticker != null) {
            Box {
                ComposerIconButton(
                    icon = Icons.Outlined.Add,
                    contentDescription = stringResource(R.string.composer_more),
                    onClick = { menuOpen = true },
                )
                DropdownMenu(expanded = menuOpen, onDismissRequest = { menuOpen = false }) {
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
                        ComposerMenuItem(Icons.Outlined.Mood, R.string.composer_sticker) {
                            menuOpen = false
                            sticker()
                        }
                    }
                }
            }
        }
        ComposerIconButton(
            icon = Icons.Outlined.TextFormat,
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

@Composable
private fun ComposerMenuItem(
    icon: ImageVector,
    label: Int,
    enabled: Boolean = true,
    onClick: () -> Unit,
) {
    DropdownMenuItem(
        text = { Text(text = stringResource(label)) },
        leadingIcon = { Icon(icon, contentDescription = null) },
        enabled = enabled,
        onClick = onClick,
    )
}

@Composable
private fun ComposerIconButton(
    icon: ImageVector,
    contentDescription: String,
    onClick: () -> Unit,
    selected: Boolean = false,
) {
    IconButton(
        onClick = onClick,
        colors = IconButtonDefaults.iconButtonColors(
            containerColor = if (selected) MaterialTheme.colorScheme.primary.copy(alpha = 0.12f) else Color.Transparent,
            contentColor = if (selected) {
                MaterialTheme.colorScheme.primary
            } else {
                MaterialTheme.colorScheme.onSurfaceVariant
            },
        ),
        modifier = Modifier.size(40.dp),
    ) {
        Icon(icon, contentDescription = contentDescription, modifier = Modifier.size(20.dp))
    }
}

/** Filled accent circle when there is something to send; quiet otherwise. */
@Composable
private fun SendButton(enabled: Boolean, onClick: () -> Unit) {
    val container by animateColorAsState(
        if (enabled) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.surfaceContainerHigh,
        animationSpec = tween(MOTION_FAST),
        label = "send-container",
    )
    val content = if (enabled) MaterialTheme.colorScheme.onPrimary else MaterialTheme.colorScheme.onSurfaceVariant
    IconButton(
        onClick = onClick,
        enabled = enabled,
        colors = IconButtonDefaults.iconButtonColors(
            containerColor = container,
            contentColor = content,
            disabledContainerColor = container,
            disabledContentColor = content,
        ),
        modifier = Modifier.size(36.dp),
    ) {
        Icon(
            Icons.AutoMirrored.Filled.Send,
            contentDescription = stringResource(R.string.composer_send),
            modifier = Modifier.size(18.dp),
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

/** One line of composer context (reply, edit, upload, a blocked command). */
@Composable
private fun ComposerNotice(
    text: String,
    icon: ImageVector?,
    cancelContentDescription: String?,
    onCancel: (() -> Unit)?,
    modifier: Modifier = Modifier,
    isError: Boolean = false,
    cancelModifier: Modifier = Modifier,
) {
    val tint = if (isError) Waddle.colors.dangerText else MaterialTheme.colorScheme.primary
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs),
        modifier = modifier
            .fillMaxWidth()
            .padding(start = 14.dp, end = Waddle.space.xxs, top = Waddle.space.xxs),
    ) {
        icon?.let { Icon(it, contentDescription = null, tint = tint, modifier = Modifier.size(16.dp)) }
        Text(
            text = text,
            style = MaterialTheme.typography.labelMedium,
            color = tint,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f),
        )
        if (onCancel != null) {
            IconButton(onClick = onCancel, modifier = cancelModifier.size(32.dp)) {
                Icon(
                    Icons.Filled.Close,
                    contentDescription = cancelContentDescription,
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.size(16.dp),
                )
            }
        }
    }
}

private val COMPOSER_RADIUS = 20.dp
private const val COMPOSER_MAX_LINES = 6
private const val MOTION_FAST = 160

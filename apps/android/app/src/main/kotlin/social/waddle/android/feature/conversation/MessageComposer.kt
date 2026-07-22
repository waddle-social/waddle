package social.waddle.android.feature.conversation

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.outlined.AttachFile
import androidx.compose.material.icons.outlined.Gif
import androidx.compose.material.icons.outlined.Mood
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
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
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch
import social.waddle.android.R
import social.waddle.android.client.ExtensionCommand
import social.waddle.android.client.MentionCandidate
import social.waddle.android.client.MentionRef

/**
 * Text input row + send button (send clears the draft optimistically).
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

    Surface(tonalElevation = 3.dp, modifier = modifier.fillMaxWidth()) {
        Column(modifier = Modifier.navigationBarsPadding()) {
            if (editing != null) {
                ComposerBanner(
                    text = stringResource(R.string.composer_editing),
                    cancelContentDescription = stringResource(R.string.composer_cancel_edit),
                    onCancel = onCancelEdit,
                )
            }
            if (replying != null && editing == null) {
                ComposerBanner(
                    text = stringResource(R.string.composer_replying_to, replying.authorName),
                    cancelContentDescription = stringResource(R.string.composer_cancel_reply),
                    onCancel = onCancelReply,
                )
            }
            when (uploadState) {
                UploadState.Idle -> Unit
                UploadState.Uploading -> ComposerBanner(
                    text = stringResource(R.string.upload_in_progress),
                    cancelContentDescription = null,
                    onCancel = null,
                )
                UploadState.TooLarge -> ComposerBanner(
                    text = stringResource(R.string.upload_too_large),
                    cancelContentDescription = stringResource(R.string.upload_dismiss),
                    onCancel = onClearUpload,
                    isError = true,
                )
                UploadState.Failed -> ComposerBanner(
                    text = stringResource(R.string.upload_failed),
                    cancelContentDescription = stringResource(R.string.upload_dismiss),
                    onCancel = onClearUpload,
                    isError = true,
                )
            }
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
                // Dismissing the hint is the mobile Esc: it disarms
                // interception for this prefix so the next send posts
                // the draft as plain text. Without it, a prefix that
                // matches no command (no popover to dismiss) would be
                // unsendable.
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(start = 16.dp, end = 4.dp, top = 4.dp)
                        .testTag(SlashCommandTestTags.BLOCKED_HINT),
                ) {
                    Text(
                        text = stringResource(R.string.composer_slash_unknown, blocked),
                        style = MaterialTheme.typography.labelMedium,
                        color = MaterialTheme.colorScheme.error,
                        modifier = Modifier.weight(1f),
                    )
                    IconButton(
                        onClick = {
                            dismissedSlashPrefix = blocked.removePrefix("/")
                            slashBlockedPrefix = null
                        },
                        modifier = Modifier.testTag(SlashCommandTestTags.BLOCKED_HINT_DISMISS),
                    ) {
                        Icon(
                            Icons.Filled.Close,
                            contentDescription = stringResource(R.string.composer_slash_send_as_text),
                        )
                    }
                }
            }
            if (mentionToken != null && mentionResults.isNotEmpty()) {
                MentionPopover(
                    candidates = mentionResults,
                    onSelect = { candidate ->
                        mentionTracker.insertMention(draft.text, mentionToken, candidate)?.let { insertion ->
                            // The tracker already synced to the inserted
                            // text; going through updateDraft would
                            // re-diff the same change.
                            draft = TextFieldValue(insertion.text, TextRange(insertion.cursor))
                            // Accepting a candidate mutates the draft like
                            // a keystroke — the XEP-0085 composing state
                            // must refresh with it.
                            onDraftChanged()
                        }
                    },
                )
            }
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 8.dp, vertical = 8.dp),
                verticalAlignment = Alignment.Bottom,
            ) {
                if (onAttach != null) {
                    IconButton(
                        onClick = onAttach,
                        enabled = uploadState != UploadState.Uploading,
                        modifier = Modifier
                            .padding(end = 4.dp)
                            .size(48.dp),
                    ) {
                        Icon(
                            Icons.Outlined.AttachFile,
                            contentDescription = stringResource(R.string.composer_attach),
                        )
                    }
                }
                if (onGif != null) {
                    IconButton(
                        onClick = onGif,
                        modifier = Modifier
                            .padding(end = 4.dp)
                            .size(48.dp),
                    ) {
                        Icon(
                            Icons.Outlined.Gif,
                            contentDescription = stringResource(R.string.composer_gif),
                        )
                    }
                }
                if (onSticker != null) {
                    IconButton(
                        onClick = onSticker,
                        modifier = Modifier
                            .padding(end = 4.dp)
                            .size(48.dp),
                    ) {
                        Icon(
                            Icons.Outlined.Mood,
                            contentDescription = stringResource(R.string.composer_sticker),
                        )
                    }
                }
                OutlinedTextField(
                    value = draft,
                    onValueChange = { value ->
                        if (value.text != draft.text) onDraftChanged()
                        updateDraft(value)
                    },
                    modifier = Modifier.weight(1f),
                    placeholder = { Text(text = stringResource(R.string.composer_placeholder)) },
                    maxLines = 5,
                )
                IconButton(
                    onClick = {
                        val trigger = if (slashDispatch != null && editing == null) {
                            parseSlashTrigger(draft.text)?.takeIf { candidate ->
                                shouldInterceptSlashSend(candidate, dismissedSlashPrefix)
                            }
                        } else {
                            null
                        }
                        if (trigger == null) {
                            onSend(draft.text, mentionTracker.mentionRefs())
                            updateDraft(TextFieldValue(""))
                        } else {
                            val command = resolveSlashCommand(trigger.prefix, slashCommands, inMuc)
                            if (command == null || slashDispatch == null) {
                                // Unresolved (or ambiguous) prefix: block the
                                // send instead of posting a broken command as
                                // a message (web slashSubmitBlocked parity).
                                slashBlockedPrefix = "/${trigger.prefix}"
                            } else {
                                val invocation = buildSlashInvocation(command, trigger.trailing)
                                slashDispatching = true
                                scope.launch {
                                    val handled = try {
                                        slashDispatch(invocation)
                                    } finally {
                                        slashDispatching = false
                                    }
                                    // Only a successful handoff clears the
                                    // draft — failure leaves slash mode armed.
                                    if (handled) updateDraft(TextFieldValue(""))
                                }
                            }
                        }
                    },
                    enabled = draft.text.isNotBlank() && !slashDispatching,
                    modifier = Modifier
                        .padding(start = 4.dp)
                        .size(48.dp),
                ) {
                    Icon(
                        Icons.AutoMirrored.Filled.Send,
                        contentDescription = stringResource(R.string.composer_send),
                    )
                }
            }
        }
    }
}

@Composable
private fun ComposerBanner(
    text: String,
    cancelContentDescription: String?,
    onCancel: (() -> Unit)?,
    isError: Boolean = false,
) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .padding(start = 16.dp, end = 4.dp, top = 4.dp),
    ) {
        Text(
            text = text,
            style = MaterialTheme.typography.labelMedium,
            color = if (isError) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.primary,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f),
        )
        if (onCancel != null) {
            IconButton(onClick = onCancel) {
                Icon(
                    Icons.Filled.Close,
                    contentDescription = cancelContentDescription,
                )
            }
        }
    }
}

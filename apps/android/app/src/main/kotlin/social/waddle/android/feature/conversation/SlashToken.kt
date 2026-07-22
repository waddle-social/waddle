package social.waddle.android.feature.conversation

import social.waddle.android.client.ExtensionCommand
import social.waddle.android.client.ExtensionCommandScope

/**
 * Pure slash-command tokenizing, matching, and dispatch planning —
 * a one-to-one port of the wasm chat client's
 * `slash-trigger.ts` / `slash-match.ts` / `slash-dispatch.ts` so both
 * composers resolve identical drafts to identical invocations.
 */

/** The parsed `/prefix trailing…` shape of a slash draft. */
data class SlashTrigger(val prefix: String, val trailing: String)

/**
 * Anchored at the start of the whole draft: a slash command is the
 * entire message, never an infix. `prefix` is empty for a bare `/`.
 */
private val SLASH_TRIGGER_PATTERN =
    Regex("""^/([a-zA-Z][a-zA-Z0-9_-]*)?(?:(\s)([\s\S]*))?$""")

fun parseSlashTrigger(text: String): SlashTrigger? {
    val match = SLASH_TRIGGER_PATTERN.matchEntire(text) ?: return null
    val prefix = match.groupValues[1]
    if (match.groups[2] == null) return SlashTrigger(prefix = prefix, trailing = "")
    return SlashTrigger(prefix = prefix, trailing = match.groupValues[3].trimStart())
}

/**
 * Autocomplete candidates for [prefix]: commands with a composer
 * prefix, channel-scoped ones only inside MUCs, matched by
 * case-insensitive starts-with.
 */
fun filterSlashCandidates(
    prefix: String,
    commands: List<ExtensionCommand>,
    inMuc: Boolean,
): List<ExtensionCommand> = commands.filter { command ->
    val composerPrefix = command.composerPrefix ?: return@filter false
    if (command.scope == ExtensionCommandScope.CHANNEL && !inMuc) return@filter false
    composerPrefix.startsWith(prefix, ignoreCase = true)
}

/**
 * Exact resolution of [prefix]. Multiple extensions advertising the
 * same prefix refuse to auto-resolve so the user picks explicitly
 * from the autocomplete popover.
 */
fun resolveSlashCommand(
    prefix: String,
    commands: List<ExtensionCommand>,
    inMuc: Boolean,
): ExtensionCommand? {
    if (prefix.isEmpty()) return null
    val matches = commands.filter { command ->
        val composerPrefix = command.composerPrefix ?: return@filter false
        if (command.scope == ExtensionCommandScope.CHANNEL && !inMuc) return@filter false
        composerPrefix.equals(prefix, ignoreCase = true)
    }
    return matches.singleOrNull()
}

/** How a resolved slash draft executes. */
sealed interface SlashInvocation {
    val command: ExtensionCommand

    /** Trailing text fills the command's inline field and submits. */
    data class InlineSubmit(
        override val command: ExtensionCommand,
        val fieldName: String,
        val value: String,
    ) : SlashInvocation

    /** A bare `/prefix` of a direct-execute command runs immediately. */
    data class DirectExecute(override val command: ExtensionCommand) : SlashInvocation

    /** Everything else opens the palette form. */
    data class OpenPalette(
        override val command: ExtensionCommand,
        val prefillFirstRequired: String? = null,
    ) : SlashInvocation
}

/** The four-way dispatch table ported from `buildSlashInvocation`. */
fun buildSlashInvocation(command: ExtensionCommand, trailing: String): SlashInvocation {
    val value = trailing.trim()
    val inlineField = command.inlineField
    return when {
        inlineField != null && value.isNotEmpty() ->
            SlashInvocation.InlineSubmit(command = command, fieldName = inlineField, value = value)
        command.composerExecute && value.isEmpty() ->
            SlashInvocation.DirectExecute(command = command)
        // Preserve unexpected trailing text by opening the palette
        // with a prefill.
        value.isNotEmpty() ->
            SlashInvocation.OpenPalette(command = command, prefillFirstRequired = value)
        else -> SlashInvocation.OpenPalette(command = command)
    }
}

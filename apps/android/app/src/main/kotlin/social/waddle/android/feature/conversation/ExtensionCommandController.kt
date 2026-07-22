package social.waddle.android.feature.conversation

import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import social.waddle.android.AppGraph
import social.waddle.android.client.ExtensionCommand
import social.waddle.android.client.ExtensionCommandAction
import social.waddle.android.client.ExtensionCommandCall
import social.waddle.android.client.ExtensionCommandField
import social.waddle.android.client.ExtensionCommandNote
import social.waddle.android.client.ExtensionCommandResult
import social.waddle.android.client.ExtensionCommandStatus
import social.waddle.android.client.ExtensionCommandSubmission
import social.waddle.android.client.ExtensionFieldType
import social.waddle.android.client.ExtensionNoteType
import social.waddle.android.client.ExtensionSubmitField
import social.waddle.android.viewModelFactoryOf

/** Node the server publishes the AI chatbot command under. */
const val AI_CHATBOT_COMMAND_NODE = "urn:waddle:extension:1:ai-chatbot"

/** UI severity of a finished (or in-flight) command stage. */
enum class CommandPhaseState { LOADING, SUCCESS, WARNING, ERROR }

data class CommandPhase(val state: CommandPhaseState, val detail: String? = null)

/** One open multi-stage form: the palette sheet renders it. */
data class ExtensionFormSession(
    val command: ExtensionCommand,
    val sessionId: String,
    val title: String?,
    val instructions: String?,
    val fields: List<ExtensionCommandField>,
    val actions: List<ExtensionCommandAction>,
    val roomJid: String?,
)

/** A finished command's outcome, surfaced as a dismissible banner. */
data class ExtensionOutcomeNotice(val commandName: String, val phase: CommandPhase)

/**
 * Slash-command invocation state machine — the port of the wasm chat
 * client's `extension-launcher.ts` dispatch path. Account-scoped
 * (keyed like [StickerPacksViewModel]); one command runs at a time in
 * v1 (slash-triggered only, no extensions toolbar).
 *
 * Semantics carried over verbatim:
 * - inline-submit stays inline ONLY when the response is a
 *   single-stage `executing` form whose advertised actions include
 *   `complete`; otherwise the palette opens with the value prefilled.
 * - the AI chatbot (`urn:waddle:extension:1:ai-chatbot`) forces its
 *   `output` field to `channel` inside rooms and posts the prompt as
 *   a normal public channel message BEFORE submitting.
 * - a failed dispatch returns `false` so the composer keeps the slash
 *   draft armed.
 */
class ExtensionCommandController(
    private val discover: suspend () -> List<ExtensionCommand>,
    private val invoke: suspend (
        serviceJid: String,
        node: String,
        roomJid: String?,
    ) -> ExtensionCommandCall,
    private val submit: suspend (ExtensionCommandSubmission) -> ExtensionCommandCall,
) : ViewModel() {
    private val _commands = MutableStateFlow<List<ExtensionCommand>>(emptyList())

    /** Discovered slash commands (empty until discovery answers). */
    val commands: StateFlow<List<ExtensionCommand>> = _commands.asStateFlow()

    private val _phases = MutableStateFlow<Map<String, CommandPhase>>(emptyMap())

    /** Per-node invocation phase (loading / outcome / error). */
    val phases: StateFlow<Map<String, CommandPhase>> = _phases.asStateFlow()

    private val _formSession = MutableStateFlow<ExtensionFormSession?>(null)

    /** The open palette form, `null` when no sheet should show. */
    val formSession: StateFlow<ExtensionFormSession?> = _formSession.asStateFlow()

    private val _lastOutcome = MutableStateFlow<ExtensionOutcomeNotice?>(null)

    /** Latest formless outcome, for a dismissible banner. */
    val lastOutcome: StateFlow<ExtensionOutcomeNotice?> = _lastOutcome.asStateFlow()

    private var discoveryInFlight = false

    /** Out-of-order guard: only the newest wire call applies state. */
    private var callEpoch = 0

    /** Fire-and-forget discovery; the session layer caches it. */
    fun ensureDiscovered() {
        if (_commands.value.isNotEmpty() || discoveryInFlight) return
        discoveryInFlight = true
        viewModelScope.launch {
            try {
                _commands.value = discover()
            } finally {
                discoveryInFlight = false
            }
        }
    }

    /**
     * Run one slash invocation. Returns `true` when the command was
     * handed off (the composer may clear the draft) and `false` on
     * failure (the slash draft stays armed for a retry).
     */
    suspend fun dispatch(
        invocation: SlashInvocation,
        roomJid: String?,
        sendPublicMessage: suspend (String) -> Unit,
    ): Boolean {
        val command = invocation.command
        val node = command.node
        val epoch = beginCall()
        setPhase(node, CommandPhase(CommandPhaseState.LOADING))
        _formSession.value = null
        _lastOutcome.value = null

        val call = invoke(command.serviceJid, node, roomJid)
        if (epoch != callEpoch) return false
        val result = when (call) {
            is ExtensionCommandCall.Failed -> {
                failStage(command, call.detail)
                return false
            }
            is ExtensionCommandCall.Ok -> call.result
        }
        val form = formSessionFrom(command, result, roomJid)

        val stage = InvokeStage(command, result, form, roomJid, epoch)
        return when (invocation) {
            is SlashInvocation.OpenPalette -> {
                val prefilled = invocation.prefillFirstRequired?.let { value ->
                    form?.let { prefillFirstRequired(it, value) }
                } ?: form
                finishStage(command, result, prefilled)
                true
            }
            is SlashInvocation.DirectExecute -> {
                finishStage(command, result, form)
                true
            }
            is SlashInvocation.InlineSubmit ->
                dispatchInlineSubmit(invocation, stage, sendPublicMessage)
        }
    }

    /** Everything the first `execute` round-trip produced. */
    private data class InvokeStage(
        val command: ExtensionCommand,
        val result: ExtensionCommandResult,
        val form: ExtensionFormSession?,
        val roomJid: String?,
        val epoch: Int,
    )

    private suspend fun dispatchInlineSubmit(
        invocation: SlashInvocation.InlineSubmit,
        stage: InvokeStage,
        sendPublicMessage: suspend (String) -> Unit,
    ): Boolean {
        // Inline-submit stays inline only when the server returned a
        // single-stage form whose advertised XEP-0050 actions include
        // `complete`. Otherwise fall back to the palette so the user
        // can drive the multi-stage flow, with the value prefilled.
        val form = stage.form
        if (form != null && ExtensionCommandAction.COMPLETE in form.actions) {
            var session = setFieldValues(form, invocation.fieldName, listOf(invocation.value))
            session = forceChannelOutputForSlashAi(stage.command, session, stage.roomJid)
            _formSession.value = session
            return submitSession(session, ExtensionCommandAction.COMPLETE, sendPublicMessage, stage.epoch)
        }
        val prefilled = form?.let { setFieldValues(it, invocation.fieldName, listOf(invocation.value)) }
        finishStage(stage.command, stage.result, prefilled)
        return true
    }

    /** Submit the open form with [action] (palette buttons). */
    suspend fun submitForm(
        action: ExtensionCommandAction,
        sendPublicMessage: suspend (String) -> Unit,
    ): Boolean {
        val session = _formSession.value ?: return false
        return submitSession(session, action, sendPublicMessage, beginCall())
    }

    private suspend fun submitSession(
        session: ExtensionFormSession,
        action: ExtensionCommandAction,
        sendPublicMessage: suspend (String) -> Unit,
        epoch: Int,
    ): Boolean {
        val command = session.command
        setPhase(command.node, CommandPhase(CommandPhaseState.LOADING))
        sendPublicPromptIfRequested(command, session, action, sendPublicMessage)
        if (epoch != callEpoch) return false
        val call = submit(
            ExtensionCommandSubmission(
                serviceJid = command.serviceJid,
                node = command.node,
                sessionId = session.sessionId,
                fields = submitFields(session.fields),
                action = action,
                roomJid = session.roomJid,
            ),
        )
        if (epoch != callEpoch) return false
        return when (call) {
            is ExtensionCommandCall.Failed -> {
                failStage(command, call.detail)
                false
            }
            is ExtensionCommandCall.Ok -> {
                finishStage(command, call.result, formSessionFrom(command, call.result, session.roomJid))
                true
            }
        }
    }

    /** Palette field edit. */
    fun updateField(name: String, values: List<String>) {
        val session = _formSession.value ?: return
        _formSession.value = setFieldValues(session, name, values)
    }

    /** Close the palette sheet without submitting. */
    fun dismissForm() {
        _formSession.value = null
    }

    fun dismissOutcome() {
        _lastOutcome.value = null
    }

    // ── Stage bookkeeping ────────────────────────────────────────────

    private fun beginCall(): Int {
        callEpoch += 1
        return callEpoch
    }

    private fun setPhase(node: String, phase: CommandPhase) {
        _phases.value = _phases.value + (node to phase)
    }

    private fun failStage(command: ExtensionCommand, detail: String?) {
        val phase = CommandPhase(CommandPhaseState.ERROR, detail)
        setPhase(command.node, phase)
        if (_formSession.value?.command?.node != command.node) {
            _lastOutcome.value = ExtensionOutcomeNotice(command.name, phase)
        }
    }

    private fun finishStage(
        command: ExtensionCommand,
        result: ExtensionCommandResult,
        form: ExtensionFormSession?,
    ) {
        val outcome = commandOutcome(result)
        setPhase(command.node, outcome)
        _formSession.value = form
        if (form == null) {
            _lastOutcome.value = ExtensionOutcomeNotice(command.name, outcome)
        }
    }

    // ── Ported pure helpers ──────────────────────────────────────────

    private fun formSessionFrom(
        command: ExtensionCommand,
        result: ExtensionCommandResult,
        roomJid: String?,
    ): ExtensionFormSession? {
        val sessionId = result.sessionId ?: return null
        val form = result.form ?: return null
        if (result.status != ExtensionCommandStatus.EXECUTING || form.fields.isEmpty()) return null
        return ExtensionFormSession(
            command = command,
            sessionId = sessionId,
            title = form.title,
            instructions = form.instructions,
            fields = form.fields,
            actions = result.actions,
            roomJid = roomJid,
        )
    }

    private fun setFieldValues(
        session: ExtensionFormSession,
        name: String,
        values: List<String>,
    ): ExtensionFormSession = session.copy(
        fields = session.fields.map { field ->
            if (field.name == name) field.copy(values = values) else field
        },
    )

    private fun prefillFirstRequired(
        session: ExtensionFormSession,
        value: String,
    ): ExtensionFormSession {
        val target = session.fields.firstOrNull { field ->
            field.required && field.type != ExtensionFieldType.HIDDEN
        } ?: return session
        return setFieldValues(session, target.name, listOf(value))
    }

    private fun forceChannelOutputForSlashAi(
        command: ExtensionCommand,
        session: ExtensionFormSession,
        roomJid: String?,
    ): ExtensionFormSession {
        if (roomJid == null || command.node != AI_CHATBOT_COMMAND_NODE) return session
        val output = session.fields.firstOrNull { it.name == "output" } ?: return session
        val supportsChannelOutput =
            output.options.isEmpty() || output.options.any { it.value == "channel" }
        return if (supportsChannelOutput) {
            setFieldValues(session, "output", listOf("channel"))
        } else {
            session
        }
    }

    private suspend fun sendPublicPromptIfRequested(
        command: ExtensionCommand,
        session: ExtensionFormSession,
        action: ExtensionCommandAction,
        sendPublicMessage: suspend (String) -> Unit,
    ) {
        if (command.node != AI_CHATBOT_COMMAND_NODE) return
        if (action == ExtensionCommandAction.CANCEL || action == ExtensionCommandAction.PREV) return
        if (fieldValue(session.fields, "output") != "channel") return
        if (session.roomJid == null) return
        val prompt = fieldValue(session.fields, "prompt")?.trim().orEmpty()
        if (prompt.isEmpty()) return
        sendPublicMessage(prompt)
    }

    private fun fieldValue(fields: List<ExtensionCommandField>, name: String): String? {
        val field = fields.firstOrNull { it.name == name } ?: return null
        return field.values.firstOrNull { it.isNotBlank() } ?: field.values.firstOrNull()
    }

    private fun submitFields(fields: List<ExtensionCommandField>): List<ExtensionSubmitField> =
        fields
            .filter { it.type != ExtensionFieldType.FIXED }
            .map { field -> ExtensionSubmitField(name = field.name, values = submitValues(field)) }

    private fun submitValues(field: ExtensionCommandField): List<String> = when (field.type) {
        // Booleans submit the canonical true/false pair (web parity).
        ExtensionFieldType.BOOLEAN -> listOf(
            if (field.values.firstOrNull() == "1" || field.values.firstOrNull() == "true") "true" else "false",
        )
        ExtensionFieldType.HIDDEN,
        ExtensionFieldType.LIST_MULTI,
        ExtensionFieldType.TEXT_MULTI,
        ExtensionFieldType.JID_MULTI,
        -> field.values
        else -> listOf(field.values.firstOrNull().orEmpty())
    }

    private fun commandOutcome(result: ExtensionCommandResult): CommandPhase {
        notesDetail(result.notes.filter { it.type == ExtensionNoteType.ERROR })?.let {
            return CommandPhase(CommandPhaseState.ERROR, it)
        }
        notesDetail(result.notes.filter { it.type == ExtensionNoteType.WARN })?.let {
            return CommandPhase(CommandPhaseState.WARNING, it)
        }
        return when (result.status) {
            ExtensionCommandStatus.EXECUTING ->
                if (result.form?.fields?.isNotEmpty() == true) {
                    CommandPhase(CommandPhaseState.WARNING)
                } else {
                    CommandPhase(CommandPhaseState.WARNING, INCOMPLETE_FORM_DETAIL)
                }
            ExtensionCommandStatus.CANCELED ->
                CommandPhase(CommandPhaseState.ERROR, CANCELED_DETAIL)
            ExtensionCommandStatus.COMPLETED ->
                CommandPhase(
                    CommandPhaseState.SUCCESS,
                    notesDetail(result.notes.filter { it.type == ExtensionNoteType.INFO }),
                )
        }
    }

    private fun notesDetail(notes: List<ExtensionCommandNote>): String? {
        val values = notes.map { it.value.trim() }.filter { it.isNotEmpty() }
        return if (values.isEmpty()) null else values.joinToString(" ")
    }

    companion object {
        /** Web-parity outcome literals (`extension-commands/result.ts`). */
        const val CANCELED_DETAIL = "Command canceled."
        const val INCOMPLETE_FORM_DETAIL =
            "Extension returned a form that this client cannot complete yet."

        fun factory(graph: AppGraph): ViewModelProvider.Factory = viewModelFactoryOf {
            val manager = graph.sessionManager
            ExtensionCommandController(
                discover = manager::discoverExtensionCommands,
                invoke = manager::invokeExtensionCommand,
                submit = manager::submitExtensionCommandForm,
            )
        }
    }
}

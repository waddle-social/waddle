package social.waddle.android.client

import social.waddle.client.ffi.WaddleAdhocAction
import social.waddle.client.ffi.WaddleAdhocStatus
import social.waddle.client.ffi.WaddleExtensionCommand
import social.waddle.client.ffi.WaddleExtensionCommandForm
import social.waddle.client.ffi.WaddleExtensionCommandFormField
import social.waddle.client.ffi.WaddleExtensionCommandNote
import social.waddle.client.ffi.WaddleExtensionCommandResult
import social.waddle.client.ffi.WaddleExtensionCommandScope
import social.waddle.client.ffi.WaddleExtensionFieldType
import social.waddle.client.ffi.WaddleExtensionFormField
import social.waddle.client.ffi.WaddleExtensionNoteType

/**
 * Domain types of the `urn:waddle:extension:1` slash-command surface
 * (XEP-0050 ad-hoc commands on the extensions component), mirroring
 * the typed FFI records one-to-one so app code never touches the
 * generated bindings.
 */

/** `waddle#command_scope` — where a command may be offered. */
enum class ExtensionCommandScope { GLOBAL, CHANNEL }

/** One discovered extension command with its composer metadata. */
data class ExtensionCommand(
    val serviceJid: String,
    val node: String,
    val name: String,
    val scope: ExtensionCommandScope,
    /** Slash trigger (without the leading `/`), e.g. `poll`. */
    val composerPrefix: String? = null,
    /** Form field the composer may fill inline from trailing text. */
    val inlineField: String? = null,
    /** Whether a bare `/prefix` executes without opening the palette. */
    val composerExecute: Boolean = false,
)

/** XEP-0050 §3 `action` attribute. */
enum class ExtensionCommandAction { EXECUTE, CANCEL, NEXT, PREV, COMPLETE }

/** XEP-0050 §3 `status` attribute on responses. */
enum class ExtensionCommandStatus { EXECUTING, COMPLETED, CANCELED }

/** XEP-0004 §3.3 field types (unknown wire types map to TEXT_SINGLE). */
enum class ExtensionFieldType {
    BOOLEAN,
    FIXED,
    HIDDEN,
    JID_MULTI,
    JID_SINGLE,
    LIST_MULTI,
    LIST_SINGLE,
    TEXT_MULTI,
    TEXT_PRIVATE,
    TEXT_SINGLE,
}

/** One `<option/>` of a list field. */
data class ExtensionFieldOption(val label: String?, val value: String)

/** One `<field/>` of a command-response form. */
data class ExtensionCommandField(
    val name: String,
    val label: String?,
    val type: ExtensionFieldType,
    val required: Boolean,
    val options: List<ExtensionFieldOption>,
    val values: List<String>,
    /**
     * Forbidden-field defense (web `isForbiddenExtensionCommandField`
     * parity, computed by the Rust parser): `text-private` fields and
     * secret-named vars never render an input or submit.
     */
    val blocked: Boolean = false,
)

/** The XEP-0004 form of a command response. */
data class ExtensionCommandForm(
    val title: String?,
    val instructions: String?,
    val fields: List<ExtensionCommandField>,
)

/** XEP-0050 §3 `<note/>` severity. */
enum class ExtensionNoteType { INFO, WARN, ERROR }

/** One `<note/>` diagnostic from a command response. */
data class ExtensionCommandNote(val type: ExtensionNoteType, val value: String)

/** A parsed extension command response. */
data class ExtensionCommandResult(
    val status: ExtensionCommandStatus,
    /** XEP-0050 `sessionid`, threaded verbatim into follow-ups. */
    val sessionId: String?,
    /** Actions the service allows for the next stage. */
    val actions: List<ExtensionCommandAction>,
    val form: ExtensionCommandForm?,
    val notes: List<ExtensionCommandNote>,
)

/** One submitted field value pair. */
data class ExtensionSubmitField(val name: String, val values: List<String>)

/** One XEP-0050 form-stage submission. */
data class ExtensionCommandSubmission(
    val serviceJid: String,
    val node: String,
    /** Threaded verbatim from the previous stage's response. */
    val sessionId: String?,
    val fields: List<ExtensionSubmitField>,
    val action: ExtensionCommandAction,
    val roomJid: String?,
)

/**
 * Typed outcome of one invoke/submit round-trip. [Failed.detail]
 * carries the stanza-error text (or condition) when the server
 * refused; `null` for transport-level failures.
 */
sealed interface ExtensionCommandCall {
    data class Ok(val result: ExtensionCommandResult) : ExtensionCommandCall
    data class Failed(val detail: String?) : ExtensionCommandCall
}

// ── FFI mappings ─────────────────────────────────────────────────────

internal fun WaddleExtensionCommand.toDomain(): ExtensionCommand = ExtensionCommand(
    serviceJid = serviceJid,
    node = node,
    name = name,
    scope = when (scope) {
        WaddleExtensionCommandScope.GLOBAL -> ExtensionCommandScope.GLOBAL
        WaddleExtensionCommandScope.CHANNEL -> ExtensionCommandScope.CHANNEL
    },
    composerPrefix = composerPrefix,
    inlineField = inlineField,
    composerExecute = composerExecute,
)

internal fun WaddleExtensionCommandResult.toDomain(): ExtensionCommandResult = ExtensionCommandResult(
    status = when (status) {
        WaddleAdhocStatus.EXECUTING -> ExtensionCommandStatus.EXECUTING
        WaddleAdhocStatus.COMPLETED -> ExtensionCommandStatus.COMPLETED
        WaddleAdhocStatus.CANCELED -> ExtensionCommandStatus.CANCELED
    },
    sessionId = sessionId,
    actions = actions.map { it.toDomain() },
    form = form?.toDomain(),
    notes = notes.map { it.toDomain() },
)

private fun WaddleAdhocAction.toDomain(): ExtensionCommandAction = when (this) {
    WaddleAdhocAction.EXECUTE -> ExtensionCommandAction.EXECUTE
    WaddleAdhocAction.CANCEL -> ExtensionCommandAction.CANCEL
    WaddleAdhocAction.NEXT -> ExtensionCommandAction.NEXT
    WaddleAdhocAction.PREV -> ExtensionCommandAction.PREV
    WaddleAdhocAction.COMPLETE -> ExtensionCommandAction.COMPLETE
}

internal fun ExtensionCommandAction.toFfi(): WaddleAdhocAction = when (this) {
    ExtensionCommandAction.EXECUTE -> WaddleAdhocAction.EXECUTE
    ExtensionCommandAction.CANCEL -> WaddleAdhocAction.CANCEL
    ExtensionCommandAction.NEXT -> WaddleAdhocAction.NEXT
    ExtensionCommandAction.PREV -> WaddleAdhocAction.PREV
    ExtensionCommandAction.COMPLETE -> WaddleAdhocAction.COMPLETE
}

private fun WaddleExtensionCommandForm.toDomain(): ExtensionCommandForm = ExtensionCommandForm(
    title = title,
    instructions = instructions,
    fields = fields.map { it.toDomain() },
)

private fun WaddleExtensionCommandFormField.toDomain(): ExtensionCommandField = ExtensionCommandField(
    name = `var`,
    label = label,
    type = when (fieldType) {
        WaddleExtensionFieldType.BOOLEAN -> ExtensionFieldType.BOOLEAN
        WaddleExtensionFieldType.FIXED -> ExtensionFieldType.FIXED
        WaddleExtensionFieldType.HIDDEN -> ExtensionFieldType.HIDDEN
        WaddleExtensionFieldType.JID_MULTI -> ExtensionFieldType.JID_MULTI
        WaddleExtensionFieldType.JID_SINGLE -> ExtensionFieldType.JID_SINGLE
        WaddleExtensionFieldType.LIST_MULTI -> ExtensionFieldType.LIST_MULTI
        WaddleExtensionFieldType.LIST_SINGLE -> ExtensionFieldType.LIST_SINGLE
        WaddleExtensionFieldType.TEXT_MULTI -> ExtensionFieldType.TEXT_MULTI
        WaddleExtensionFieldType.TEXT_PRIVATE -> ExtensionFieldType.TEXT_PRIVATE
        WaddleExtensionFieldType.TEXT_SINGLE -> ExtensionFieldType.TEXT_SINGLE
    },
    required = required,
    options = options.map { ExtensionFieldOption(label = it.label, value = it.value) },
    values = values,
    blocked = blocked,
)

private fun WaddleExtensionCommandNote.toDomain(): ExtensionCommandNote = ExtensionCommandNote(
    type = when (noteType) {
        WaddleExtensionNoteType.INFO -> ExtensionNoteType.INFO
        WaddleExtensionNoteType.WARN -> ExtensionNoteType.WARN
        WaddleExtensionNoteType.ERROR -> ExtensionNoteType.ERROR
    },
    value = value,
)

internal fun ExtensionSubmitField.toFfi(): WaddleExtensionFormField =
    WaddleExtensionFormField(`var` = name, values = values)

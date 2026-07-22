package social.waddle.android.feature.conversation

import androidx.compose.material3.SnackbarHostState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.res.stringResource
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import kotlinx.coroutines.launch
import social.waddle.android.AppGraph
import social.waddle.android.R

/**
 * Slash-command wiring a graph-backed host hands to
 * [ConversationScreen]. Kept optional there so plain scaffold tests
 * (no [AppGraph] provided) render without the feature.
 */
data class SlashCommandHost(
    val controller: ExtensionCommandController,
    /** The MUC the composer sits in; `null` for DMs. */
    val roomJid: String?,
)

/** Compose test tags shared with tests. */
object SlashCommandTestTags {
    const val POPOVER = "slash-command-popover"
    const val DISMISS = "slash-command-dismiss"
    const val BLOCKED_HINT = "slash-command-blocked-hint"
    const val FORM_SHEET = "slash-command-form-sheet"

    fun item(node: String) = "slash-command-item:$node"
}

/** The composer's dispatch seam bound to one host + send pipeline. */
fun slashDispatchOf(
    host: SlashCommandHost,
    sendPublicMessage: suspend (String) -> Unit,
): suspend (SlashInvocation) -> Boolean = { invocation ->
    host.controller.dispatch(invocation, host.roomJid, sendPublicMessage)
}

private const val EXTENSION_COMMANDS_VIEW_MODEL_KEY = "extension-commands"

/**
 * Account-scoped [ExtensionCommandController] ([StickerPacksViewModel]
 * keying precedent): the activity retains VM stores across
 * logout/login, and account A's in-flight command must not serve
 * account B.
 */
@Composable
fun rememberExtensionCommandController(graph: AppGraph): ExtensionCommandController {
    val session by graph.currentSession.collectAsStateWithLifecycle()
    return viewModel(
        key = "$EXTENSION_COMMANDS_VIEW_MODEL_KEY:${session?.jid.orEmpty()}",
        factory = ExtensionCommandController.factory(graph),
    )
}

/**
 * The command surfaces outside the composer: the palette form sheet
 * for the open multi-stage session, and formless outcomes (success
 * details, warnings, errors — including server `<note/>` text)
 * surfaced through the screen snackbar.
 */
@Composable
fun SlashCommandSurfaces(
    host: SlashCommandHost,
    snackbarHostState: SnackbarHostState,
    sendPublicMessage: suspend (String) -> Unit,
) {
    val controller = host.controller
    val phases by controller.phases.collectAsStateWithLifecycle()
    val formSession by controller.formSession.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()

    formSession?.let { session ->
        ExtensionCommandFormSheet(
            session = session,
            phase = phases[session.command.node],
            onFieldChange = controller::updateField,
            onAction = { action ->
                scope.launch { controller.submitForm(action, sendPublicMessage) }
            },
            onDismiss = controller::dismissForm,
        )
    }

    val failedText = stringResource(R.string.extension_command_failed)
    val completedText = stringResource(R.string.extension_command_completed)
    LaunchedEffect(controller) {
        controller.lastOutcome.collect { notice ->
            if (notice == null) return@collect
            val fallback = when (notice.phase.state) {
                CommandPhaseState.SUCCESS -> completedText
                else -> failedText
            }
            snackbarHostState.showSnackbar(
                notice.phase.detail ?: fallback.format(notice.commandName),
            )
            controller.dismissOutcome()
        }
    }
}

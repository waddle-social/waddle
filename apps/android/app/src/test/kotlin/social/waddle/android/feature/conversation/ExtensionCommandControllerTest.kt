package social.waddle.android.feature.conversation

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import social.waddle.android.client.ExtensionCommand
import social.waddle.android.client.ExtensionCommandAction
import social.waddle.android.client.ExtensionCommandCall
import social.waddle.android.client.ExtensionCommandField
import social.waddle.android.client.ExtensionCommandForm
import social.waddle.android.client.ExtensionCommandNote
import social.waddle.android.client.ExtensionCommandResult
import social.waddle.android.client.ExtensionCommandScope
import social.waddle.android.client.ExtensionCommandStatus
import social.waddle.android.client.ExtensionFieldOption
import social.waddle.android.client.ExtensionFieldType
import social.waddle.android.client.ExtensionNoteType
import social.waddle.android.client.ExtensionSubmitField

/**
 * The `extension-launcher.ts` dispatch-port semantics: the
 * stays-inline rule, the palette fallback with prefills, the AI
 * output-forcing + public-prompt pre-send order, and failure leaving
 * slash mode armed (dispatch returns `false`).
 */
@OptIn(ExperimentalCoroutinesApi::class)
class ExtensionCommandControllerTest {
    private val dispatcher = StandardTestDispatcher()

    @Before
    fun setUp() {
        Dispatchers.setMain(dispatcher)
    }

    @After
    fun tearDown() {
        Dispatchers.resetMain()
    }

    private data class SubmitCall(
        val node: String,
        val sessionId: String?,
        val fields: List<ExtensionSubmitField>,
        val action: ExtensionCommandAction,
        val roomJid: String?,
    )

    private class Wire {
        var commands: List<ExtensionCommand> = emptyList()
        var invokeAnswer: ExtensionCommandCall =
            ExtensionCommandCall.Ok(completedResult())
        var submitAnswer: ExtensionCommandCall =
            ExtensionCommandCall.Ok(completedResult())
        val invokeCalls = mutableListOf<Triple<String, String, String?>>()
        val submitCalls = mutableListOf<SubmitCall>()
        val log = mutableListOf<String>()

        fun controller() = ExtensionCommandController(
            discover = { commands },
            invoke = { serviceJid, node, roomJid ->
                invokeCalls += Triple(serviceJid, node, roomJid)
                log += "invoke:$node"
                invokeAnswer
            },
            submit = { submission ->
                submitCalls += SubmitCall(
                    node = submission.node,
                    sessionId = submission.sessionId,
                    fields = submission.fields,
                    action = submission.action,
                    roomJid = submission.roomJid,
                )
                log += "submit:${submission.node}"
                submitAnswer
            },
        )
    }

    private companion object {
        fun completedResult(notes: List<ExtensionCommandNote> = emptyList()) =
            ExtensionCommandResult(
                status = ExtensionCommandStatus.COMPLETED,
                sessionId = null,
                actions = emptyList(),
                form = null,
                notes = notes,
            )

        fun executingForm(
            fields: List<ExtensionCommandField>,
            actions: List<ExtensionCommandAction> =
                listOf(ExtensionCommandAction.COMPLETE, ExtensionCommandAction.CANCEL),
            sessionId: String = "s-1",
        ) = ExtensionCommandResult(
            status = ExtensionCommandStatus.EXECUTING,
            sessionId = sessionId,
            actions = actions,
            form = ExtensionCommandForm(title = null, instructions = null, fields = fields),
            notes = emptyList(),
        )

        fun textField(name: String, required: Boolean = false) = ExtensionCommandField(
            name = name,
            label = null,
            type = ExtensionFieldType.TEXT_SINGLE,
            required = required,
            options = emptyList(),
            values = emptyList(),
        )

        fun command(
            node: String = "urn:waddle:extension:1:decision-polls",
            inlineField: String? = null,
            composerExecute: Boolean = false,
        ) = ExtensionCommand(
            serviceJid = "extensions.waddle.test",
            node = node,
            name = "Test Command",
            scope = ExtensionCommandScope.GLOBAL,
            composerPrefix = "test",
            inlineField = inlineField,
            composerExecute = composerExecute,
        )

        val noSend: suspend (String) -> Unit = { }
    }

    @Test
    fun `inline submit stays inline when the executing form allows complete`() = runTest(dispatcher.scheduler) {
        val wire = Wire()
        val ai = command(inlineField = "prompt")
        wire.invokeAnswer = ExtensionCommandCall.Ok(executingForm(listOf(textField("prompt"))))
        wire.submitAnswer = ExtensionCommandCall.Ok(completedResult())
        val controller = wire.controller()

        val handled = controller.dispatch(
            SlashInvocation.InlineSubmit(command = ai, fieldName = "prompt", value = "hello"),
            roomJid = null,
            sendPublicMessage = noSend,
        )

        assertTrue(handled)
        val submitted = wire.submitCalls.single()
        assertEquals("s-1", submitted.sessionId)
        assertEquals(ExtensionCommandAction.COMPLETE, submitted.action)
        assertEquals(
            listOf(ExtensionSubmitField(name = "prompt", values = listOf("hello"))),
            submitted.fields,
        )
        // Completed without a follow-up form: the sheet never shows.
        assertNull(controller.formSession.value)
        assertEquals(
            CommandPhaseState.SUCCESS,
            controller.phases.value.getValue(ai.node).state,
        )
    }

    @Test
    fun `inline submit falls back to the palette when complete is not allowed`() = runTest(dispatcher.scheduler) {
        val wire = Wire()
        val ai = command(inlineField = "prompt")
        wire.invokeAnswer = ExtensionCommandCall.Ok(
            executingForm(
                listOf(textField("prompt")),
                actions = listOf(ExtensionCommandAction.NEXT, ExtensionCommandAction.CANCEL),
            ),
        )
        val controller = wire.controller()

        val handled = controller.dispatch(
            SlashInvocation.InlineSubmit(command = ai, fieldName = "prompt", value = "hello"),
            roomJid = null,
            sendPublicMessage = noSend,
        )

        assertTrue(handled)
        assertTrue(wire.submitCalls.isEmpty())
        val session = controller.formSession.value ?: error("palette form expected")
        // The inline value is prefilled so nothing the user typed is lost.
        assertEquals(listOf("hello"), session.fields.single { it.name == "prompt" }.values)
        assertEquals(
            listOf(ExtensionCommandAction.NEXT, ExtensionCommandAction.CANCEL),
            session.actions,
        )
    }

    @Test
    fun `open palette prefills the first required visible field`() = runTest(dispatcher.scheduler) {
        val wire = Wire()
        val poll = command()
        wire.invokeAnswer = ExtensionCommandCall.Ok(
            executingForm(
                listOf(
                    ExtensionCommandField(
                        name = "kind",
                        label = null,
                        type = ExtensionFieldType.HIDDEN,
                        required = true,
                        options = emptyList(),
                        values = listOf("poll"),
                    ),
                    textField("question", required = true),
                ),
            ),
        )
        val controller = wire.controller()

        val handled = controller.dispatch(
            SlashInvocation.OpenPalette(command = poll, prefillFirstRequired = "Lunch?"),
            roomJid = "general@muc.waddle.test",
            sendPublicMessage = noSend,
        )

        assertTrue(handled)
        val session = controller.formSession.value ?: error("palette form expected")
        assertEquals(listOf("Lunch?"), session.fields.single { it.name == "question" }.values)
        assertEquals(listOf("poll"), session.fields.single { it.name == "kind" }.values)
        assertEquals("general@muc.waddle.test", session.roomJid)
    }

    @Test
    fun `direct execute completes without a form`() = runTest(dispatcher.scheduler) {
        val wire = Wire()
        val stargate = command(composerExecute = true)
        wire.invokeAnswer = ExtensionCommandCall.Ok(
            completedResult(
                notes = listOf(ExtensionCommandNote(ExtensionNoteType.INFO, "Indeed.")),
            ),
        )
        val controller = wire.controller()

        val handled = controller.dispatch(
            SlashInvocation.DirectExecute(command = stargate),
            roomJid = "general@muc.waddle.test",
            sendPublicMessage = noSend,
        )

        assertTrue(handled)
        assertNull(controller.formSession.value)
        assertEquals(
            ExtensionOutcomeNotice(
                commandName = "Test Command",
                phase = CommandPhase(CommandPhaseState.SUCCESS, "Indeed."),
            ),
            controller.lastOutcome.value,
        )
        assertEquals("general@muc.waddle.test", wire.invokeCalls.single().third)
    }

    @Test
    fun `ai inline submit forces channel output and posts the prompt before submitting`() =
        runTest(dispatcher.scheduler) {
            val wire = Wire()
            val ai = command(node = AI_CHATBOT_COMMAND_NODE, inlineField = "prompt")
            wire.invokeAnswer = ExtensionCommandCall.Ok(
                executingForm(
                    listOf(
                        textField("prompt"),
                        ExtensionCommandField(
                            name = "output",
                            label = null,
                            type = ExtensionFieldType.LIST_SINGLE,
                            required = false,
                            options = listOf(
                                ExtensionFieldOption(label = null, value = "private"),
                                ExtensionFieldOption(label = null, value = "channel"),
                            ),
                            values = listOf("private"),
                        ),
                    ),
                ),
            )
            val controller = wire.controller()
            val sent = mutableListOf<String>()

            val handled = controller.dispatch(
                SlashInvocation.InlineSubmit(command = ai, fieldName = "prompt", value = "hi ai"),
                roomJid = "general@muc.waddle.test",
                sendPublicMessage = { body ->
                    sent += body
                    wire.log += "public:$body"
                },
            )

            assertTrue(handled)
            assertEquals(listOf("hi ai"), sent)
            // The public prompt precedes the submit on the wire.
            assertEquals(
                listOf("invoke:${ai.node}", "public:hi ai", "submit:${ai.node}"),
                wire.log,
            )
            val submitted = wire.submitCalls.single()
            assertEquals(
                listOf("channel"),
                submitted.fields.single { it.name == "output" }.values,
            )
        }

    @Test
    fun `ai inline submit outside a room neither forces output nor posts publicly`() =
        runTest(dispatcher.scheduler) {
            val wire = Wire()
            val ai = command(node = AI_CHATBOT_COMMAND_NODE, inlineField = "prompt")
            wire.invokeAnswer = ExtensionCommandCall.Ok(
                executingForm(
                    listOf(
                        textField("prompt"),
                        ExtensionCommandField(
                            name = "output",
                            label = null,
                            type = ExtensionFieldType.LIST_SINGLE,
                            required = false,
                            options = listOf(ExtensionFieldOption(label = null, value = "channel")),
                            values = listOf("private"),
                        ),
                    ),
                ),
            )
            val controller = wire.controller()
            val sent = mutableListOf<String>()

            controller.dispatch(
                SlashInvocation.InlineSubmit(command = ai, fieldName = "prompt", value = "hi"),
                roomJid = null,
                sendPublicMessage = { sent += it },
            )

            assertTrue(sent.isEmpty())
            assertEquals(
                listOf("private"),
                wire.submitCalls.single().fields.single { it.name == "output" }.values,
            )
        }

    @Test
    fun `a failed invoke reports the error and leaves slash mode armed`() = runTest(dispatcher.scheduler) {
        val wire = Wire()
        val poll = command()
        wire.invokeAnswer = ExtensionCommandCall.Failed(detail = "not allowed here")
        val controller = wire.controller()

        val handled = controller.dispatch(
            SlashInvocation.OpenPalette(command = poll),
            roomJid = null,
            sendPublicMessage = noSend,
        )

        assertFalse(handled)
        assertEquals(
            CommandPhase(CommandPhaseState.ERROR, "not allowed here"),
            controller.phases.value.getValue(poll.node),
        )
        assertEquals(
            ExtensionOutcomeNotice(
                commandName = "Test Command",
                phase = CommandPhase(CommandPhaseState.ERROR, "not allowed here"),
            ),
            controller.lastOutcome.value,
        )
        assertNull(controller.formSession.value)
    }

    @Test
    fun `error notes on a completed result surface as an error outcome`() = runTest(dispatcher.scheduler) {
        val wire = Wire()
        val stargate = command(composerExecute = true)
        wire.invokeAnswer = ExtensionCommandCall.Ok(
            completedResult(
                notes = listOf(ExtensionCommandNote(ExtensionNoteType.ERROR, "Gate is closed.")),
            ),
        )
        val controller = wire.controller()

        controller.dispatch(
            SlashInvocation.DirectExecute(command = stargate),
            roomJid = null,
            sendPublicMessage = noSend,
        )

        assertEquals(
            CommandPhase(CommandPhaseState.ERROR, "Gate is closed."),
            controller.phases.value.getValue(stargate.node),
        )
    }

    @Test
    fun `palette submit cancel closes the form without submitting fields`() = runTest(dispatcher.scheduler) {
        val wire = Wire()
        val poll = command()
        wire.invokeAnswer = ExtensionCommandCall.Ok(executingForm(listOf(textField("question"))))
        val controller = wire.controller()
        controller.dispatch(
            SlashInvocation.OpenPalette(command = poll),
            roomJid = null,
            sendPublicMessage = noSend,
        )
        wire.submitAnswer = ExtensionCommandCall.Ok(
            ExtensionCommandResult(
                status = ExtensionCommandStatus.CANCELED,
                sessionId = "s-1",
                actions = emptyList(),
                form = null,
                notes = emptyList(),
            ),
        )

        val handled = controller.submitForm(ExtensionCommandAction.CANCEL, noSend)

        assertTrue(handled)
        assertNull(controller.formSession.value)
        assertEquals(
            CommandPhase(CommandPhaseState.ERROR, ExtensionCommandController.CANCELED_DETAIL),
            controller.phases.value.getValue(poll.node),
        )
    }

    @Test
    fun `updateField edits the open palette form`() = runTest(dispatcher.scheduler) {
        val wire = Wire()
        val poll = command()
        wire.invokeAnswer = ExtensionCommandCall.Ok(executingForm(listOf(textField("question"))))
        val controller = wire.controller()
        controller.dispatch(
            SlashInvocation.OpenPalette(command = poll),
            roomJid = null,
            sendPublicMessage = noSend,
        )

        controller.updateField("question", listOf("Dinner?"))

        assertEquals(
            listOf("Dinner?"),
            controller.formSession.value?.fields?.single()?.values,
        )
    }

    @Test
    fun `ensureDiscovered loads once and exposes the commands`() = runTest(dispatcher.scheduler) {
        val wire = Wire()
        wire.commands = listOf(command())
        val controller = wire.controller()

        controller.ensureDiscovered()
        runCurrent()

        assertEquals(wire.commands, controller.commands.value)
    }
}

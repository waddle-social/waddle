package social.waddle.android.client

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.client.ffi.WaddleAdhocAction
import social.waddle.client.ffi.WaddleAdhocStatus
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddleException
import social.waddle.client.ffi.WaddleExtensionCommand
import social.waddle.client.ffi.WaddleExtensionCommandForm
import social.waddle.client.ffi.WaddleExtensionCommandFormField
import social.waddle.client.ffi.WaddleExtensionCommandNote
import social.waddle.client.ffi.WaddleExtensionCommandResult
import social.waddle.client.ffi.WaddleExtensionCommandScope
import social.waddle.client.ffi.WaddleExtensionFieldType
import social.waddle.client.ffi.WaddleExtensionNoteType

/**
 * `urn:waddle:extension:1` slash-command verbs through the manager:
 * the once-per-session discovery cache, its generation gate, and the
 * typed invoke/submit passthroughs.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class XmppSessionManagerExtensionCommandsTest {
    private class Harness(testScope: TestScope) {
        val factory = FakeClientFactory()
        val manager = XmppSessionManager(
            sessionPrefs = SessionPrefs(InMemoryPreferencesDataStore()),
            clientFactory = factory,
            networkSignal = FakeNetworkSignal(),
            userPrefs = UserPrefs(InMemoryPreferencesDataStore()),
            reconnectPolicy = ReconnectPolicy(PinnedRandom(0.5)),
            dispatcher = StandardTestDispatcher(testScope.testScheduler),
        )

        suspend fun loginReady(scope: TestScope) {
            manager.login(testSessionInfo())
            scope.runCurrent()
            factory.emit(WaddleClientEvent.Connected)
            scope.runCurrent()
        }

        val client get() = factory.clients.last()
    }

    private fun ffiCommand(
        node: String = "urn:waddle:extension:1:decision-polls",
        prefix: String? = "poll",
    ) = WaddleExtensionCommand(
        serviceJid = "extensions.waddle.test",
        node = node,
        name = "Decision Polls",
        scope = WaddleExtensionCommandScope.CHANNEL,
        composerPrefix = prefix,
        inlineField = null,
        composerExecute = false,
    )

    private fun domainCommand(
        node: String = "urn:waddle:extension:1:decision-polls",
        prefix: String? = "poll",
    ) = ExtensionCommand(
        serviceJid = "extensions.waddle.test",
        node = node,
        name = "Decision Polls",
        scope = ExtensionCommandScope.CHANNEL,
        composerPrefix = prefix,
        inlineField = null,
        composerExecute = false,
    )

    @Test
    fun `discovery maps the command set and caches it for the session`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.extensionCommands.commands = listOf(ffiCommand())

        assertEquals(listOf(domainCommand()), harness.manager.discoverExtensionCommands())
        assertEquals(
            listOf(domainCommand()),
            harness.manager.extensionCommandStore.commands.value,
        )

        // Cached: a second discovery never rewalks the disco tree.
        assertEquals(listOf(domainCommand()), harness.manager.discoverExtensionCommands())
        assertEquals(1, harness.client.extensionCommands.discoverCalls)
        harness.manager.logout()
    }

    @Test
    fun `an empty discovery is not cached and the next call retries`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)

        assertEquals(emptyList<ExtensionCommand>(), harness.manager.discoverExtensionCommands())
        assertNull(harness.manager.extensionCommandStore.commands.value)

        harness.client.extensionCommands.commands = listOf(ffiCommand())
        assertEquals(listOf(domainCommand()), harness.manager.discoverExtensionCommands())
        assertEquals(2, harness.client.extensionCommands.discoverCalls)
        harness.manager.logout()
    }

    @Test
    fun `a failed discovery degrades to an empty set and retries later`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.extensionCommands.discoverFailure = RuntimeException("boom")

        assertEquals(emptyList<ExtensionCommand>(), harness.manager.discoverExtensionCommands())
        assertNull(harness.manager.extensionCommandStore.commands.value)

        harness.client.extensionCommands.discoverFailure = null
        harness.client.extensionCommands.commands = listOf(ffiCommand())
        assertEquals(listOf(domainCommand()), harness.manager.discoverExtensionCommands())
        harness.manager.logout()
    }

    @Test
    fun `logout clears the discovery cache`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.extensionCommands.commands = listOf(ffiCommand())
        harness.manager.discoverExtensionCommands()

        harness.manager.logout()

        assertNull(harness.manager.extensionCommandStore.commands.value)
    }

    private fun ffiExecutingPollForm() = WaddleExtensionCommandResult(
        status = WaddleAdhocStatus.EXECUTING,
        sessionId = "s-1",
        actions = listOf(WaddleAdhocAction.COMPLETE, WaddleAdhocAction.CANCEL),
        form = WaddleExtensionCommandForm(
            title = "New Poll",
            instructions = null,
            fields = listOf(
                WaddleExtensionCommandFormField(
                    `var` = "question",
                    label = "Question",
                    fieldType = WaddleExtensionFieldType.TEXT_SINGLE,
                    required = true,
                    blocked = false,
                    options = emptyList(),
                    values = emptyList(),
                ),
            ),
        ),
        notes = listOf(
            WaddleExtensionCommandNote(
                noteType = WaddleExtensionNoteType.INFO,
                value = "Fill in the poll.",
            ),
        ),
    )

    @Test
    fun `invoke maps the executing form response into domain types`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.extensionCommands.invokeResult = ffiExecutingPollForm()

        val call = harness.manager.invokeExtensionCommand(
            serviceJid = "extensions.waddle.test",
            node = "urn:waddle:extension:1:decision-polls",
            roomJid = "general@muc.waddle.test",
        )

        val result = (call as ExtensionCommandCall.Ok).result
        assertEquals(ExtensionCommandStatus.EXECUTING, result.status)
        assertEquals("s-1", result.sessionId)
        assertEquals(
            listOf(ExtensionCommandAction.COMPLETE, ExtensionCommandAction.CANCEL),
            result.actions,
        )
        assertEquals("New Poll", result.form?.title)
        assertEquals(
            ExtensionCommandField(
                name = "question",
                label = "Question",
                type = ExtensionFieldType.TEXT_SINGLE,
                required = true,
                options = emptyList(),
                values = emptyList(),
            ),
            result.form?.fields?.single(),
        )
        assertEquals(
            listOf(ExtensionCommandNote(ExtensionNoteType.INFO, "Fill in the poll.")),
            result.notes,
        )
        assertEquals(
            Triple(
                "extensions.waddle.test",
                "urn:waddle:extension:1:decision-polls",
                "general@muc.waddle.test",
            ),
            harness.client.extensionCommands.invokeCalls.single(),
        )
        harness.manager.logout()
    }

    @Test
    fun `submit threads the session id fields action and room`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)

        val call = harness.manager.submitExtensionCommandForm(
            ExtensionCommandSubmission(
                serviceJid = "extensions.waddle.test",
                node = "urn:waddle:extension:1:ai-chatbot",
                sessionId = "s-9",
                fields = listOf(ExtensionSubmitField(name = "prompt", values = listOf("hello"))),
                action = ExtensionCommandAction.COMPLETE,
                roomJid = "general@muc.waddle.test",
            ),
        )

        assertTrue(call is ExtensionCommandCall.Ok)
        val recorded = harness.client.extensionCommands.submitCalls.single()
        assertEquals("s-9", recorded.sessionId)
        assertEquals(WaddleAdhocAction.COMPLETE, recorded.action)
        assertEquals("prompt", recorded.fields.single().`var`)
        assertEquals(listOf("hello"), recorded.fields.single().values)
        assertEquals("general@muc.waddle.test", recorded.roomJid)
        harness.manager.logout()
    }

    @Test
    fun `a stanza refusal collapses to a failed call with its text`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.extensionCommands.invokeFailure =
            WaddleException.Stanza(condition = "forbidden", text = "not yours")

        val call = harness.manager.invokeExtensionCommand(
            serviceJid = "extensions.waddle.test",
            node = "urn:waddle:extension:1:decision-polls",
        )

        assertEquals(ExtensionCommandCall.Failed(detail = "not yours"), call)
        harness.manager.logout()
    }

    @Test
    fun `verbs with no live session fail typed`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()

        assertEquals(emptyList<ExtensionCommand>(), harness.manager.discoverExtensionCommands())
        assertEquals(
            ExtensionCommandCall.Failed(detail = null),
            harness.manager.invokeExtensionCommand("extensions.waddle.test", "n"),
        )
        harness.manager.logout()
    }
}

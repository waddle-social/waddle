package social.waddle.android.client

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import social.waddle.android.client.session.ActiveSession
import social.waddle.android.client.store.SessionStores
import social.waddle.client.ffi.WaddleClientInterface
import social.waddle.client.ffi.WaddleException
import social.waddle.client.ffi.WaddleExtensionCommandResult

/**
 * `urn:waddle:extension:1` slash-command passthroughs (XEP-0050
 * ad-hoc commands on the extensions component). Same shape rules as
 * [StickerVerbs]: never throws past the seam — every call collapses
 * to a typed result.
 *
 * Discovery is cached once per session in
 * [SessionStores.extensionCommandStore] (the FFI walk is several IQ
 * round-trips); the post-wire store write is generation-gated
 * ([ProfileVerbs] precedent) so a slow discovery landing after a
 * relogin can never park a stale command set into the next session's
 * store. Failed or empty discoveries are not cached — the next call
 * retries.
 */
internal class ExtensionCommandVerbs(
    private val activeSession: ActiveSession,
    private val stores: SessionStores,
) {
    private val discoveryMutex = Mutex()

    suspend fun discoverExtensionCommands(): List<ExtensionCommand> {
        stores.extensionCommandStore.commands.value?.let { return it }
        return discoveryMutex.withLock {
            stores.extensionCommandStore.commands.value ?: runDiscovery()
        }
    }

    private suspend fun runDiscovery(): List<ExtensionCommand> {
        val generation = activeSession.generation
        val commands = try {
            when (val result = activeSession.invoke { client ->
                client.discoverExtensionCommands().map { it.toDomain() }
            }) {
                ActiveSession.Invocation.NotConnected -> return emptyList()
                is ActiveSession.Invocation.Completed -> result.value
            }
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            return emptyList()
        }
        if (commands.isNotEmpty() && activeSession.generation == generation) {
            stores.extensionCommandStore.applyDiscovered(commands)
        }
        return commands
    }

    suspend fun invokeExtensionCommand(
        serviceJid: String,
        node: String,
        roomJid: String?,
    ): ExtensionCommandCall = call { client ->
        client.invokeExtensionCommand(serviceJid, node, roomJid)
    }

    suspend fun submitExtensionCommandForm(
        submission: ExtensionCommandSubmission,
    ): ExtensionCommandCall = call { client ->
        client.submitExtensionCommandForm(
            serviceJid = submission.serviceJid,
            node = submission.node,
            sessionId = submission.sessionId,
            fields = submission.fields.map { it.toFfi() },
            action = submission.action.toFfi(),
            roomJid = submission.roomJid,
        )
    }

    private suspend fun call(
        op: suspend (WaddleClientInterface) -> WaddleExtensionCommandResult,
    ): ExtensionCommandCall {
        return try {
            when (val result = activeSession.invoke(op)) {
                ActiveSession.Invocation.NotConnected -> ExtensionCommandCall.Failed(detail = null)
                is ActiveSession.Invocation.Completed -> ExtensionCommandCall.Ok(result.value.toDomain())
            }
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (stanza: WaddleException.Stanza) {
            ExtensionCommandCall.Failed(detail = stanza.text ?: stanza.condition)
        } catch (_: Throwable) {
            ExtensionCommandCall.Failed(detail = null)
        }
    }
}

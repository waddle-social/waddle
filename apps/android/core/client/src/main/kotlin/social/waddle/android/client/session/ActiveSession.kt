package social.waddle.android.client.session

import kotlinx.coroutines.CancellationException
import social.waddle.android.client.XmppEventBridge
import social.waddle.client.ffi.WaddleClientInterface
import social.waddle.client.ffi.WaddleSendMessageOutcome
import social.waddle.client.ffi.WaddleSmResumeState

/**
 * Shared per-attempt session state: which FFI client (if any) is live,
 * whose account it serves, and the standard call shapes the UI
 * passthroughs use against it. Attempts never overlap (the connection
 * loop is sequential), so plain volatile set-on-ready /
 * clear-on-teardown fields are race-free.
 */
internal class ActiveSession(
    private val onResumeState: (WaddleSmResumeState?) -> Unit,
) {
    /**
     * The client of the attempt that reached `SessionReady`, while that
     * attempt is alive — the target of the UI passthroughs.
     */
    @Volatile
    var client: WaddleClientInterface? = null
        private set

    @Volatile
    var ownBareJid: String? = null

    /** The live attempt's bridge, for injecting locally-produced events. */
    @Volatile
    var bridge: XmppEventBridge? = null
        private set

    /** XEP-0490 publish-options probe result, reset per attempt. */
    @Volatile
    var mdsPublishSupported: Boolean? = null

    /** XEP-0363 upload service JID, discovered once per attempt. */
    @Volatile
    var uploadService: String? = null

    /** Fresh bridge for a new attempt (the FFI client is one-shot). */
    fun beginAttempt(): XmppEventBridge {
        val attemptBridge = XmppEventBridge(onResumeState)
        bridge = attemptBridge
        return attemptBridge
    }

    /** The attempt reached `SessionReady`: expose its client, reset probes. */
    fun onReady(readyClient: WaddleClientInterface) {
        client = readyClient
        mdsPublishSupported = null
        uploadService = null
    }

    /** The attempt ended; passthroughs fall back to their not-connected shape. */
    fun endAttempt() {
        client = null
    }

    /** Boolean client verb with the standard not-connected/error → false. */
    suspend fun clientCall(op: suspend (WaddleClientInterface) -> Boolean): Boolean {
        val liveClient = client ?: return false
        return try {
            op(liveClient)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            false
        }
    }

    /** Message send shape: no client → `NotConnected`, throw → `TransportError`. */
    suspend fun send(
        op: suspend (WaddleClientInterface) -> WaddleSendMessageOutcome,
    ): WaddleSendMessageOutcome {
        val liveClient = client ?: return WaddleSendMessageOutcome.NotConnected
        return try {
            op(liveClient)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            WaddleSendMessageOutcome.TransportError
        }
    }

    /** Nullable fetch shape: `null` when no session is ready or the call threw. */
    suspend fun <T : Any> fetch(op: suspend (WaddleClientInterface) -> T): T? {
        val liveClient = client ?: return null
        return try {
            op(liveClient)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            null
        }
    }
}

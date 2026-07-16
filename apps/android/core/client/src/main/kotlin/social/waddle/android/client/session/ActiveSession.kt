package social.waddle.android.client.session

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import social.waddle.android.client.VerbResult
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
    /** Serializes attempt replacement against generation-fenced FFI sends. */
    private val attemptMutex = Mutex()

    data class Attempt(
        val bridge: XmppEventBridge,
        val connectionGeneration: Long,
    )

    /**
     * The client of the attempt that reached `SessionReady`, while that
     * attempt is alive — the target of the UI passthroughs.
     */
    @Volatile
    var client: WaddleClientInterface? = null
        private set

    @Volatile
    var ownBareJid: String? = null

    @Volatile
    var connectionGeneration: Long = 0
        private set

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
    suspend fun beginAttempt(): Attempt = attemptMutex.withLock {
        connectionGeneration += 1
        client = null
        val attemptBridge = XmppEventBridge(onResumeState)
        bridge = attemptBridge
        Attempt(attemptBridge, connectionGeneration)
    }

    /** The attempt reached `SessionReady`: expose its client, reset probes. */
    suspend fun onReady(readyClient: WaddleClientInterface, generation: Long) = attemptMutex.withLock {
        if (generation != connectionGeneration) return@withLock
        client = readyClient
        mdsPublishSupported = null
        uploadService = null
    }

    /** The attempt ended; passthroughs fall back to their not-connected shape. */
    suspend fun endAttempt(generation: Long) = attemptMutex.withLock {
        if (generation == connectionGeneration) client = null
    }

    /** Fire-and-check verb shape: no client → [VerbResult.NotConnected],
     *  a refusal or a broken transport → [VerbResult.Rejected]. */
    suspend fun verbCall(op: suspend (WaddleClientInterface) -> Boolean): VerbResult {
        val liveClient = client ?: return VerbResult.NotConnected
        return try {
            if (op(liveClient)) VerbResult.Ok else VerbResult.Rejected
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            VerbResult.Rejected
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

    /** Generation-fenced message send used by the durable outbound queue.
     * The caller claims its row under [expectedGeneration] before invoking
     * FFI; a replacement attempt therefore cannot inherit the old claim. */
    suspend fun sendAtGeneration(
        expectedGeneration: Long,
        op: suspend (WaddleClientInterface) -> WaddleSendMessageOutcome,
    ): WaddleSendMessageOutcome = attemptMutex.withLock {
        val liveClient = client
        if (liveClient == null || connectionGeneration != expectedGeneration) {
            return@withLock WaddleSendMessageOutcome.NotConnected
        }
        try {
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

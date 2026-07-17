package social.waddle.android.client.session

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import social.waddle.android.client.VerbResult
import social.waddle.android.client.XmppEventBridge
import social.waddle.android.client.prefs.DeliveryAttemptRef
import social.waddle.android.client.prefs.DeliveryAttemptTransition
import social.waddle.client.ffi.WaddleClientInterface
import social.waddle.client.ffi.WaddleSendMessageOutcome

/**
 * Shared per-attempt session state: which FFI client (if any) is live,
 * whose account it serves, and the standard call shapes the UI
 * passthroughs use against it. Attempts never overlap (the connection
 * loop is sequential), so plain volatile set-on-ready /
 * clear-on-teardown fields are race-free.
 */
internal class ActiveSession {
    /** Serializes attempt replacement against generation-fenced FFI sends. */
    private val attemptMutex = Mutex()

    data class Attempt(
        val bridge: XmppEventBridge,
        val ref: DeliveryAttemptRef,
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
    var attemptRef: DeliveryAttemptRef? = null
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
    suspend fun beginAttempt(
        attempt: DeliveryAttemptRef,
    ): Attempt = attemptMutex.withLock {
        client = null
        attemptRef = attempt
        val attemptBridge = XmppEventBridge()
        bridge = attemptBridge
        Attempt(attemptBridge, attempt)
    }

    /** The attempt reached `SessionReady`: expose its client, reset probes. */
    suspend fun onReady(
        readyClient: WaddleClientInterface,
        expectedAttempt: DeliveryAttemptRef,
    ) = attemptMutex.withLock {
        if (expectedAttempt != attemptRef) return@withLock
        client = readyClient
        mdsPublishSupported = null
        uploadService = null
    }

    /** The attempt ended; passthroughs fall back to their not-connected shape. */
    suspend fun endAttempt(expectedAttempt: DeliveryAttemptRef) = attemptMutex.withLock {
        if (expectedAttempt == attemptRef) {
            client = null
            attemptRef = null
            bridge = null
        }
    }

    /**
     * Resume failure keeps the same native client but rotates its durable
     * callback fence before fresh fallback events are accepted.
     */
    suspend fun acceptResumeTransition(
        transition: DeliveryAttemptTransition,
    ): Boolean = attemptMutex.withLock {
        when (attemptRef) {
            transition.fresh -> true
            transition.old -> {
                attemptRef = transition.fresh
                true
            }
            else -> false
        }
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
    suspend fun sendAtAttempt(
        expectedAttempt: DeliveryAttemptRef,
        op: suspend (WaddleClientInterface) -> WaddleSendMessageOutcome,
    ): WaddleSendMessageOutcome = attemptMutex.withLock {
        val liveClient = client
        if (liveClient == null || attemptRef != expectedAttempt) {
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

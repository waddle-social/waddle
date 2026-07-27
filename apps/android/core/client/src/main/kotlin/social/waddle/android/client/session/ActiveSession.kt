package social.waddle.android.client.session

import kotlinx.coroutines.CancellationException
import social.waddle.android.client.VerbResult
import social.waddle.android.client.XmppEventBridge
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
    /** Result of a generation-fenced message send attempt. */
    sealed interface LeaseSendResult {
        /** The lease was stale before a transport could be selected. */
        data object Stale : LeaseSendResult

        /** The current attempt selected its transport and produced [outcome]. */
        data class Attempted(
            val outcome: WaddleSendMessageOutcome,
        ) : LeaseSendResult
    }

    /**
     * Immutable authority to mutate or use one account attempt. Capturing
     * the bare owner alone is insufficient: logout followed by a login as
     * the same account must still fence work that was parked by the old
     * attempt.
     */
    data class OwnerLease(
        val ownerBareJid: String,
        val generation: Long,
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

    /**
     * Monotonic account-session generation: bumped on every login and
     * sign-out. Post-ack store writes capture it before the wire call
     * and compare before writing, so a slow reply that lands after a
     * logout (or a relogin — even into the SAME account, where a bare
     * JID comparison would falsely pass) can never park stale state
     * into the next session's stores.
     */
    @Volatile
    var generation: Long = 0L
        private set

    /** Called under the manager's lifecycle mutex on login/sign-out. */
    fun advanceGeneration() {
        generation += 1
    }

    /** Capture the current account authority for a durable operation. */
    fun captureOwnerLease(): OwnerLease? = ownBareJid?.let { owner ->
        OwnerLease(ownerBareJid = owner, generation = generation)
    }

    /** True only while [lease] still names this exact account attempt. */
    fun isCurrent(lease: OwnerLease): Boolean =
        ownBareJid == lease.ownerBareJid && generation == lease.generation

    /**
     * The attempt's FULL JID (account bare JID + bound resource) —
     * the XEP-0166 initiator/responder identity and the XEP-0353
     * tie-break comparand. Set when the attempt's config is built and
     * left in place after the attempt ends (an `Ended` call slot may
     * still reference it); cleared on logout.
     */
    @Volatile
    var ownFullJid: String? = null

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
        val attemptBridge = XmppEventBridge()
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

    /**
     * Lease-fenced message-send shape. The validation occurs immediately
     * before selecting the live transport, so work parked before logout or
     * a same-account relogin cannot write on the new session's behalf.
     */
    suspend fun sendIfCurrent(
        lease: OwnerLease,
        op: suspend (WaddleClientInterface) -> WaddleSendMessageOutcome,
    ): LeaseSendResult {
        // Capture the attempt's transport before validating its lease.  The
        // validation fences logout/relogin, while the captured reference
        // prevents a successor's `onReady` from being re-read and used by a
        // send that began for its predecessor.
        val liveClient = client ?: return LeaseSendResult.Attempted(WaddleSendMessageOutcome.NotConnected)
        if (!isCurrent(lease)) return LeaseSendResult.Stale
        return try {
            LeaseSendResult.Attempted(op(liveClient))
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            LeaseSendResult.Attempted(WaddleSendMessageOutcome.TransportError)
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

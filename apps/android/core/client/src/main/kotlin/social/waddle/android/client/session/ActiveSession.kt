package social.waddle.android.client.session

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.Semaphore
import kotlinx.coroutines.sync.withLock
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
    /**
     * The linearization point between ordinary transport use and account
     * revocation (logout, relogin, terminal auth failure).
     *
     * It is a readers/writer fence, not a mutex. Ordinary operations hold a
     * shared permit for the whole FFI invocation, so an operation that
     * observed a live lease owns that client until it returns; revocation
     * takes every permit, so it waits for in-flight work and no new work
     * starts until it is done. Ordinary operations do NOT serialise against
     * each other: the Rust client handles concurrent IQs, and a 30-second
     * history query must never hold up a send, a typing state or a read
     * marker.
     *
     * Attempt bookkeeping (publish/end of one physical connection) does not
     * change account authority and only takes [stateLock], so a dead
     * stream's teardown never waits behind calls that are timing out on it.
     *
     * Callers may hold `OutboundMessenger.sendMutex` before this fence.  No
     * code under this fence acquires that mutex or `lifecycleMutex`, FFI
     * calls must not synchronously re-enter the manager, and fenced
     * operations must not nest.
     */
    private val transportFence = RevocationFence()

    /** Guards the per-attempt slots below; never held across a suspension in FFI. */
    private val stateLock = Mutex()

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
     * Private identity for one physical connection attempt.  It carries the
     * owner lease captured before config/client construction so an old bridge
     * can never publish itself after logout or a same-account relogin.
     */
    data class Attempt internal constructor(
        val lease: OwnerLease,
        val bridge: XmppEventBridge,
    )

    /**
     * The retired connection that may send only the bounded call
     * teardown during logout. It is deliberately separate from the
     * outbound authority below: ordinary verbs and message sends cannot
     * discover this client after revocation.
     */
    data class RetiredCallConnection(
        val client: WaddleClientInterface,
        val ownFullJid: String?,
    )

    /**
     * The client of the attempt that reached `SessionReady`, while that
     * attempt is alive — the target of the UI passthroughs.
     */
    /**
     * The ready attempt's client.  This is deliberately private: every
     * ordinary XMPP operation must enter through [invoke], which holds the
     * same fence as logout/relogin retirement.  A nullable public client
     * made it possible for a caller to retain an old transport after the
     * check and call it after logout had installed a successor.
     */
    @Volatile
    private var client: WaddleClientInterface? = null

    /** Result of an ordinary, transport-fenced client invocation. */
    sealed interface Invocation<out T> {
        /** Logout/revocation won before a client could be selected. */
        data object NotConnected : Invocation<Nothing>

        /** The active attempt owned the transport through this invocation. */
        data class Completed<T>(val value: T) : Invocation<T>
    }

    /**
     * Result of an ordinary invocation bound to one exact account attempt.
     * Unlike [Invocation], this cannot select a replacement client's
     * transport after a logout/relogin changed the owner lease.
     */
    sealed interface LeaseInvocation<out T> {
        /** The owner changed before a transport could be selected. */
        data object Stale : LeaseInvocation<Nothing>

        /** The lease remains current but no ready client is available. */
        data object NotConnected : LeaseInvocation<Nothing>

        /** The lease owned the selected transport until [value] completed. */
        data class Completed<T>(val value: T) : LeaseInvocation<T>
    }

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

    /**
     * The sole authority used to start durable outbound work. Publishing
     * or revoking it is one volatile operation, rather than a sequence of
     * owner/generation field reads that logout could expose halfway.
     */
    @Volatile
    private var outboundOwner: OwnerLease? = null

    /**
     * Account-authority transitions: wait out every in-flight operation,
     * then mutate under [stateLock] so a concurrent attempt publish/end
     * observes either the old or the new authority, never half of each.
     */
    private suspend inline fun <T> revoking(crossinline block: () -> T): T =
        transportFence.exclusive { stateLock.withLock { block() } }

    /** Called under [revoking] by lifecycle transitions. */
    private fun advanceGenerationLocked() {
        generation += 1
        outboundOwner = null
    }

    /** Fence a fresh login or terminal session failure against active sends. */
    suspend fun advanceGeneration() = revoking {
        advanceGenerationLocked()
    }

    /**
     * Atomically invalidate [lease] only when it is still the account
     * attempt currently authorized by this session. Terminal authentication
     * handling uses this instead of a check followed by [advanceGeneration]:
     * a same-account relogin must not let a delayed old failure retire the
     * successor between those two operations.
     */
    suspend fun advanceGenerationIfCurrent(lease: OwnerLease): Boolean = revoking {
        if (outboundOwner != lease) return@revoking false
        advanceGenerationLocked()
        true
    }

    /** Publish a new account's authority after login has finished clearing old state. */
    suspend fun activateOwner(ownerBareJid: String) = revoking {
        ownBareJid = ownerBareJid
        outboundOwner = OwnerLease(ownerBareJid = ownerBareJid, generation = generation)
    }

    /**
     * Revoke ordinary outbound authority before logout can suspend. The
     * returned connection is call-teardown-only; it is never reinstalled
     * as the active transport.
     */
    suspend fun revokeOutboundAuthority(): RetiredCallConnection? = revoking {
        val retired = client?.let { RetiredCallConnection(it, ownFullJid) }
        advanceGenerationLocked()
        ownBareJid = null
        ownFullJid = null
        client = null
        retired
    }

    /** Capture the current account authority for a durable operation. */
    fun captureOwnerLease(): OwnerLease? = outboundOwner

    /** True only while [lease] still names this exact account attempt. */
    fun isCurrent(lease: OwnerLease): Boolean = outboundOwner == lease

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

    /**
     * Capture an exact owner lease before config/client construction.  The
     * bridge remains attempt-private until [publishReady] linearizes it with
     * the client and full JID under [transportFence].
     */
    suspend fun beginAttempt(): Attempt? = stateLock.withLock {
        val lease = outboundOwner ?: return@withLock null
        Attempt(lease, XmppEventBridge())
    }

    /**
     * Atomically publish all ready-only state for [attempt].  The callback
     * starts the ready pipeline while the same fence still proves ownership;
     * a revoked or old attempt does not expose a bridge/client/JID and must be
     * closed by its caller without touching a successor.
     */
    suspend fun publishReady(
        attempt: Attempt,
        readyClient: WaddleClientInterface,
        readyOwnFullJid: String,
        onPublished: () -> Unit,
    ): Boolean = stateLock.withLock {
        if (!isCurrent(attempt.lease)) return@withLock false
        bridge = attempt.bridge
        client = readyClient
        ownFullJid = readyOwnFullJid
        mdsPublishSupported = null
        uploadService = null
        onPublished()
        true
    }

    /**
     * The attempt ended; only its own client may clear the live slot. A
     * delayed old attempt must never erase a successor that has reached
     * ready state.
     */
    suspend fun endAttempt(attempt: Attempt, endingClient: WaddleClientInterface) = stateLock.withLock {
        // Both the lease and client identity must match.  An old attempt can
        // finish after a same-account relogin, so comparing the client alone
        // is insufficient to protect the successor's slot.
        if (isCurrent(attempt.lease) && client === endingClient) client = null
    }

    /**
     * The only ordinary gateway to a live FFI client.  The fence remains
     * held until [op] completes: an invocation that wins may finish on its
     * selected client, while a logout that wins first revokes the slot and
     * guarantees [op] is never called on either the retired or successor
     * client.  Callers must not call this recursively.
     */
    suspend fun <T> invoke(
        op: suspend (WaddleClientInterface) -> T,
    ): Invocation<T> = transportFence.shared {
        val liveClient = client ?: return@shared Invocation.NotConnected
        Invocation.Completed(op(liveClient))
    }

    /**
     * Invoke only while [lease] still names the exact ready account attempt.
     * The transport fence covers validation, client selection, and the FFI
     * call, so a stale operation can neither use the retired client nor hop
     * to a same-account relogin's replacement client.
     */
    suspend fun <T> invokeIfCurrent(
        lease: OwnerLease,
        op: suspend (WaddleClientInterface) -> T,
    ): LeaseInvocation<T> = transportFence.shared {
        if (!isCurrent(lease)) return@shared LeaseInvocation.Stale
        val liveClient = client ?: return@shared LeaseInvocation.NotConnected
        LeaseInvocation.Completed(op(liveClient))
    }

    /**
     * Apply a synchronous store projection while the same owner lease is
     * current. Logout waits on this fence before it clears stores, preventing
     * a completed old read from repopulating a successor's state.
     */
    suspend fun applyIfCurrent(lease: OwnerLease, action: () -> Unit): Boolean = transportFence.shared {
        if (!isCurrent(lease)) return@shared false
        action()
        true
    }

    /**
     * Run a suspending owner-scoped effect while the transport fence still
     * names [lease]. This is for durable session projections: completing an
     * old write after logout could otherwise restore state into a successor.
     */
    suspend fun runIfCurrent(lease: OwnerLease, action: suspend () -> Unit): Boolean = transportFence.shared {
        if (!isCurrent(lease)) return@shared false
        action()
        true
    }

    /**
     * Run a durable owner-scoped effect and retain its typed result while the
     * transport fence proves the exact generation. This lets a caller order
     * one persistence transaction with logout revocation without retaining a
     * live transport or waiting on broader lifecycle work.
     */
    suspend fun <T> runIfCurrentResult(
        lease: OwnerLease,
        action: suspend () -> T,
    ): LeaseInvocation<T> = transportFence.shared {
        if (!isCurrent(lease)) return@shared LeaseInvocation.Stale
        LeaseInvocation.Completed(action())
    }

    /** Fenced readiness probe for control flow that must not retain a client. */
    suspend fun hasActiveClient(): Boolean = invoke { true } is Invocation.Completed

    /** Fire-and-check verb shape: no client → [VerbResult.NotConnected],
     *  a refusal or a broken transport → [VerbResult.Rejected]. */
    suspend fun verbCall(op: suspend (WaddleClientInterface) -> Boolean): VerbResult {
        return try {
            when (val result = invoke(op)) {
                Invocation.NotConnected -> VerbResult.NotConnected
                is Invocation.Completed -> if (result.value) VerbResult.Ok else VerbResult.Rejected
            }
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            VerbResult.Rejected
        }
    }

    /**
     * [verbCall] bound to one exact account attempt.  A caller which has
     * already projected optimistic state must use this shape rather than
     * selecting whatever client happened to become ready after it parked.
     */
    suspend fun verbCallIfCurrent(
        lease: OwnerLease,
        op: suspend (WaddleClientInterface) -> Boolean,
    ): VerbResult {
        return try {
            when (val result = invokeIfCurrent(lease, op)) {
                LeaseInvocation.Stale,
                LeaseInvocation.NotConnected,
                -> VerbResult.NotConnected
                is LeaseInvocation.Completed -> if (result.value) VerbResult.Ok else VerbResult.Rejected
            }
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
        return try {
            when (val result = invoke(op)) {
                Invocation.NotConnected -> WaddleSendMessageOutcome.NotConnected
                is Invocation.Completed -> result.value
            }
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
    ): LeaseSendResult = transportFence.shared {
        if (!isCurrent(lease)) return@shared LeaseSendResult.Stale
        // The shared permit stays held across `op`: either logout revoked
        // first and this returns Stale without calling a retired client, or
        // this invocation owns the selected client and logout waits for it.
        val liveClient = client ?: return@shared LeaseSendResult.Attempted(WaddleSendMessageOutcome.NotConnected)
        try {
            LeaseSendResult.Attempted(op(liveClient))
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            LeaseSendResult.Attempted(WaddleSendMessageOutcome.TransportError)
        }
    }

    /** Nullable fetch shape: `null` when no session is ready or the call threw. */
    suspend fun <T : Any> fetch(op: suspend (WaddleClientInterface) -> T): T? {
        return try {
            when (val result = invoke(op)) {
                Invocation.NotConnected -> null
                is Invocation.Completed -> result.value
            }
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            null
        }
    }

    /** Nullable [fetch] bound to one exact account attempt. */
    suspend fun <T : Any> fetchIfCurrent(
        lease: OwnerLease,
        op: suspend (WaddleClientInterface) -> T,
    ): T? {
        return try {
            when (val result = invokeIfCurrent(lease, op)) {
                LeaseInvocation.Stale,
                LeaseInvocation.NotConnected,
                -> null
                is LeaseInvocation.Completed -> result.value
            }
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            null
        }
    }
}

/**
 * Readers/writer fence over a fair [Semaphore]. Shared holders take one
 * permit; an exclusive holder takes all of them, one by one, under
 * [writers] so two exclusive acquirers cannot deadlock each other with
 * partial sets. The semaphore is FIFO, so once an exclusive acquirer is
 * waiting, later shared acquirers queue behind it: revocation cannot be
 * starved by a stream of sends.
 */
internal class RevocationFence(@PublishedApi internal val permits: Int = DEFAULT_PERMITS) {
    @PublishedApi internal val semaphore = Semaphore(permits)

    @PublishedApi internal val writers = Mutex()

    suspend inline fun <T> shared(block: () -> T): T {
        semaphore.acquire()
        try {
            return block()
        } finally {
            semaphore.release()
        }
    }

    suspend inline fun <T> exclusive(block: () -> T): T = writers.withLock {
        var held = 0
        try {
            repeat(permits) {
                semaphore.acquire()
                held += 1
            }
            block()
        } finally {
            repeat(held) { semaphore.release() }
        }
    }

    private companion object {
        /** Upper bound on concurrent FFI operations; far above real fan-out. */
        const val DEFAULT_PERMITS = 64
    }
}

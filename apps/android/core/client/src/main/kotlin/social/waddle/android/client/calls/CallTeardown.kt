package social.waddle.android.client.calls

import kotlinx.coroutines.flow.MutableStateFlow
import social.waddle.android.client.bareJid
import social.waddle.client.ffi.WaddleCallSessionTerminateOutcome
import social.waddle.client.ffi.WaddleJingleReason

/**
 * Owns the call-slot transition and wire effects for local teardown.
 * Kept separate from [CallStore]'s event reducer so lifecycle fencing
 * cannot grow that already-dense reducer further.
 */
internal class CallTeardown(
    private val stateLock: Any,
    private val state: MutableStateFlow<CallState>,
    private val muc: MucCallEngine,
    private val signaling: CallSignaling,
    private val ownFullJid: () -> String?,
    private val callbacks: CallTeardownCallbacks,
) {
    /**
     * Claim the local slot before waiting for an in-flight transport verb.
     * This lets explicit hang-up win a concurrent remote effect's state race;
     * [sendClaimedHangUp] later validates the same connection before sending.
     */
    fun claimHangUp(reason: WaddleJingleReason): CallTeardownRequest? {
        val sender = signaling.captureActiveConnection() ?: return null
        return synchronized(stateLock) {
            val live = state.value.connectionOrNull
            if (sender.lease != null && !live.isSameActiveAttempt(sender)) return@synchronized null
            callbacks.cancelTimers()
            val claimed = state.value
            state.value = CallState.Idle
            CallTeardownRequest(claimed, sender, ownFullJid(), reason)
        }
    }

    /** Send the bounded teardown only while the connection attempt remains current. */
    suspend fun sendClaimedHangUp(request: CallTeardownRequest) {
        if (!request.sender.applyIfCurrent {}) return
        sendCurrentTeardown(request.claimed, request.sender, request.ownJid, request.reason)
    }

    /** Logout-only: the retired transport is intentionally not a [CallConnection]. */
    suspend fun hangUpWith(sender: LogoutCallTeardown, ownJid: String?) {
        var current: CallState? = null
        synchronized(stateLock) {
            callbacks.cancelTimers()
            current = state.value
            state.value = CallState.Idle
        }
        when (val claimed = current ?: return) {
            is CallState.Active -> if (claimed.kind == CallKind.MUC) {
                muc.teardownActiveForLogout(claimed, sender, ownJid)
            } else {
                terminateActiveForLogout(claimed, sender)
            }
            is CallState.MucPending -> muc.teardownPendingForLogout(claimed, sender)
            is CallState.Outgoing -> {
                if (!sender.retractForLogout(bareJid(claimed.to), claimed.sid)) {
                    callbacks.reportError("call retract failed")
                }
            }
            is CallState.Incoming -> if (claimed.accepting) {
                if (!sender.cancelAcceptingCallForLogout(claimed.from, claimed.sid)) {
                    callbacks.reportError("call finish failed")
                }
            } else if (!sender.rejectForLogout(claimed.from, claimed.sid)) {
                callbacks.reportError("call reject failed")
            }
            else -> Unit
        }
    }

    private suspend fun sendCurrentTeardown(
        claimed: CallState,
        sender: CallConnection,
        ownJid: String?,
        reason: WaddleJingleReason,
    ) {
        when (claimed) {
            is CallState.Active -> if (claimed.kind == CallKind.MUC) {
                muc.teardownActiveWithCurrent(claimed, sender, ownJid)
            } else {
                terminateActive(claimed, reason, sender)
            }
            is CallState.MucPending -> muc.teardownPendingWithCurrent(claimed, sender)
            is CallState.Outgoing -> {
                if (!sender.retract(bareJid(claimed.to), claimed.sid)) {
                    callbacks.reportError("call retract failed")
                }
            }
            is CallState.Incoming -> if (claimed.accepting) {
                if (!sender.finishWithReason(claimed.from, claimed.sid, WaddleJingleReason.CANCEL)) {
                    callbacks.reportError("call finish failed")
                }
            } else if (!sender.reject(claimed.from, claimed.sid)) {
                callbacks.reportError("call reject failed")
            }
            else -> Unit
        }
    }

    suspend fun hangUpActiveIf(sid: String, reason: WaddleJingleReason) {
        val connection = signaling.captureActiveConnection() ?: return
        var current: CallState.Active? = null
        if (!connection.applyIfCurrent {
                synchronized(stateLock) {
                    val active = state.value
                    if (active !is CallState.Active ||
                        active.sid != sid ||
                        !active.connection.isSameActiveAttempt(connection)
                    ) {
                        return@applyIfCurrent
                    }
                    callbacks.cancelTimers()
                    current = active
                    state.value = CallState.Idle
                }
            }
        ) {
                return
            }
        val claimed = current ?: return
        if (claimed.kind == CallKind.MUC) {
            muc.teardownActiveWithCurrent(claimed, connection, null)
        } else {
            terminateActive(claimed, reason, connection)
        }
    }

    fun dismiss() {
        synchronized(stateLock) {
            if (state.value !is CallState.Ended) return
            callbacks.cancelTimers()
            state.value = CallState.Idle
            callbacks.clearError()
        }
    }

    private suspend fun terminateActive(current: CallState.Active, reason: WaddleJingleReason, sender: CallConnection) {
        val outcome = sender.sessionTerminateWithOutcome(current.peer, current.sid, reason)
        if (outcome == WaddleCallSessionTerminateOutcome.ERROR) {
            callbacks.reportError("call session terminate failed")
            return
        }
        if (!sender.finish(current.peer, current.sid)) callbacks.reportError("call finish failed")
    }

    private suspend fun terminateActiveForLogout(current: CallState.Active, sender: LogoutCallTeardown) {
        val outcome = sender.terminateCallForLogout(current.peer, current.sid)
        if (outcome == WaddleCallSessionTerminateOutcome.ERROR) {
            callbacks.reportError("call session terminate failed")
            return
        }
        if (!sender.finishTerminatedCallForLogout(current.peer, current.sid)) {
            callbacks.reportError("call finish failed")
        }
    }
}

/** A local hang-up already claimed the slot; its wire teardown remains attempt-pinned. */
internal data class CallTeardownRequest(
    val claimed: CallState,
    val sender: CallConnection,
    val ownJid: String?,
    val reason: WaddleJingleReason,
)

internal data class CallTeardownCallbacks(
    val cancelTimers: () -> Unit,
    val reportError: (String) -> Unit,
    val clearError: () -> Unit,
)

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
    suspend fun hangUp(reason: WaddleJingleReason) {
        val connection = signaling.captureActiveConnection() ?: return
        hangUpWith(connection, ownFullJid(), reason)
    }

    suspend fun hangUpWith(sender: CallConnection, ownJid: String?, reason: WaddleJingleReason) {
        var current: CallState? = null
        if (!sender.applyIfCurrent {
                synchronized(stateLock) {
                    val live = state.value.connectionOrNull
                    if (sender.lease != null && !live.isSameActiveAttempt(sender)) return@applyIfCurrent
                    callbacks.cancelTimers()
                    current = state.value
                    state.value = CallState.Idle
                }
            }) return
        val claimed = current ?: return
        when (claimed) {
            is CallState.Active -> if (claimed.kind == CallKind.MUC) {
                muc.teardownActiveWith(claimed, sender, ownJid)
            } else {
                terminateActive(claimed, reason, sender)
            }
            is CallState.MucPending -> muc.teardownPendingWith(claimed, sender)
            is CallState.Outgoing ->
                if (!sender.retract(bareJid(claimed.to), claimed.sid)) callbacks.reportError("call retract failed")
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
            }) return
        val claimed = current ?: return
        if (claimed.kind == CallKind.MUC) muc.teardownActiveWith(claimed, connection, null) else terminateActive(claimed, reason, connection)
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
}

internal data class CallTeardownCallbacks(
    val cancelTimers: () -> Unit,
    val reportError: (String) -> Unit,
    val clearError: () -> Unit,
)

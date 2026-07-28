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
    suspend fun hangUp(reason: WaddleJingleReason) = hangUpWith(signaling, ownFullJid(), reason)

    suspend fun hangUpWith(sender: CallSignaling, ownJid: String?, reason: WaddleJingleReason) {
        val current: CallState
        synchronized(stateLock) {
            callbacks.cancelTimers()
            current = state.value
            state.value = CallState.Idle
        }
        when (current) {
            is CallState.Active -> if (current.kind == CallKind.MUC) {
                muc.teardownActiveWith(current, sender, ownJid)
            } else {
                terminateActive(current, reason, sender)
            }
            is CallState.MucPending -> muc.teardownPendingWith(current, sender)
            is CallState.Outgoing ->
                if (!sender.retract(bareJid(current.to), current.sid)) callbacks.reportError("call retract failed")
            is CallState.Incoming -> if (current.accepting) {
                if (!sender.finishWithReason(current.from, current.sid, WaddleJingleReason.CANCEL)) {
                    callbacks.reportError("call finish failed")
                }
            } else if (!sender.reject(current.from, current.sid)) {
                callbacks.reportError("call reject failed")
            }
            else -> Unit
        }
    }

    suspend fun hangUpActiveIf(sid: String, reason: WaddleJingleReason) {
        val current: CallState.Active
        synchronized(stateLock) {
            val active = state.value
            if (active !is CallState.Active || active.sid != sid) return
            callbacks.cancelTimers()
            current = active
            state.value = CallState.Idle
        }
        if (current.kind == CallKind.MUC) muc.teardownActive(current) else terminateActive(current, reason, signaling)
    }

    fun dismiss() {
        synchronized(stateLock) {
            if (state.value !is CallState.Ended) return
            callbacks.cancelTimers()
            state.value = CallState.Idle
            callbacks.clearError()
        }
    }

    private suspend fun terminateActive(current: CallState.Active, reason: WaddleJingleReason, sender: CallSignaling) {
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

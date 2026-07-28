package social.waddle.android.client.calls

import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import social.waddle.android.client.bareJid
import social.waddle.client.ffi.WaddleJingleReason

/**
 * Owns the bounded XEP-0353/Jingle timer effects. State ownership remains in
 * [CallStore] behind the named [CallTimerHost] operations.
 */
internal class CallTimers(
    private val outgoingTimeoutMillis: Long,
    private val sessionAcceptTimeoutMillis: Long,
    private val host: CallTimerHost,
) {
    private var outgoing: Job? = null
    private var session: Job? = null

    fun scheduleOutgoing(sid: String) {
        if (host.outgoing(sid) == null) return
        outgoing?.cancel()
        outgoing = host.launch {
            delay(outgoingTimeoutMillis)
            timeOutOutgoing(sid)
        }
    }

    fun scheduleSessionAccept(peerFullJid: String, sid: String) {
        if (host.outgoing(sid) == null) return
        session?.cancel()
        session = host.launch {
            delay(sessionAcceptTimeoutMillis)
            timeOutSessionAccept(peerFullJid, sid)
        }
    }

    fun scheduleSessionInitiate(peerFullJid: String, sid: String) {
        if (host.incoming(sid) == null) return
        session?.cancel()
        session = host.launch {
            delay(sessionAcceptTimeoutMillis)
            timeOutSessionInitiate(peerFullJid, sid)
        }
    }

    fun cancelOutgoing() {
        outgoing?.cancel()
        outgoing = null
    }

    fun cancelAll() {
        cancelOutgoing()
        session?.cancel()
        session = null
    }

    private suspend fun timeOutOutgoing(sid: String) {
        val target = host.outgoing(sid) ?: return
        val connection = target.connection ?: return
        if (!connection.applyIfCurrent { host.endOutgoingIfCurrent(sid, connection) }) return
        if (!connection.retract(bareJid(target.to), sid)) host.reportError("call retract failed")
    }

    private suspend fun timeOutSessionAccept(peerFullJid: String, sid: String) {
        val target = host.outgoing(sid) ?: return
        val connection = target.connection ?: return
        if (!connection.applyIfCurrent { host.endOutgoingIfCurrent(sid, connection) }) return
        if (!connection.sessionTerminate(peerFullJid, sid, WaddleJingleReason.TIMEOUT)) {
            host.reportError("call session terminate failed")
        }
    }

    private suspend fun timeOutSessionInitiate(peerFullJid: String, sid: String) {
        val target = host.incoming(sid) ?: return
        val connection = target.connection ?: return
        if (!connection.applyIfCurrent { host.endIncomingIfCurrent(sid, connection) }) return
        if (!connection.finishWithReason(peerFullJid, sid, WaddleJingleReason.TIMEOUT)) {
            host.reportError("call finish failed")
        }
    }
}

/** Narrow state port for [CallTimers]; it does not expose synchronization or flows. */
internal interface CallTimerHost {
    fun outgoing(sid: String): CallState.Outgoing?
    fun incoming(sid: String): CallState.Incoming?
    fun launch(block: suspend () -> Unit): Job?
    fun endOutgoingIfCurrent(sid: String, connection: CallConnection)
    fun endIncomingIfCurrent(sid: String, connection: CallConnection)
    fun reportError(message: String)
}

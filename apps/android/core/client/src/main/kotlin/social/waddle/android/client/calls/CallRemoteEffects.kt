package social.waddle.android.client.calls

import social.waddle.client.ffi.WaddleCallEvent
import social.waddle.client.ffi.WaddleCallEventKind

/**
 * Remote call-event effects run after [CallStore] has synchronously reduced the
 * slot. It owns only the XEP-0353/Jingle progression sends; the store remains
 * the sole owner of call state, lifecycle, timers, and effect ordering.
 */
internal class CallRemoteEffects(
    private val muc: MucCallEngine,
    private val ownFullJid: () -> String?,
    private val host: CallRemoteEffectsHost,
) {
    suspend fun handle(event: WaddleCallEvent, prev: CallState, connection: CallConnection?) {
        when (val kind = event.kind) {
            is WaddleCallEventKind.Propose -> host.handleProposeSideEffect(event, kind, prev, connection)
            is WaddleCallEventKind.Proceed -> proceedSideEffect(event, prev)
            is WaddleCallEventKind.SessionInitiate -> sessionInitiateSideEffect(event, kind, prev)
            // The DM reducer leaves MUC phases untouched: a matching
            // remote end event runs the engine-owned teardown instead.
            is WaddleCallEventKind.Reject -> muc.endFromRemote(
                event.sid,
                if (tieBreakExpired(kind.tieBreak, kind.reason)) CallEndReason.Expired else CallEndReason.Rejected,
            )
            is WaddleCallEventKind.Retract -> muc.endFromRemote(
                event.sid,
                if (tieBreakExpired(kind.tieBreak, kind.reason)) CallEndReason.Expired else CallEndReason.Retracted,
            )
            is WaddleCallEventKind.Finish -> muc.endFromRemote(event.sid, CallEndReason.Finished(kind.reason))
            is WaddleCallEventKind.SessionTerminate ->
                muc.endFromRemote(event.sid, CallEndReason.Finished(kind.reason))
            else -> Unit
        }
    }

    private suspend fun proceedSideEffect(event: WaddleCallEvent, prev: CallState) {
        if (prev !is CallState.Outgoing || prev.sid != event.sid) return
        val connection = prev.connection ?: return
        // A hang-up (or a hang-up plus a NEW outgoing call) that landed
        // while this effect was queued already retracted this sid; a
        // stale session-initiate must not go out for it — and its
        // failure must not clobber whatever owns the slot now.
        if (!host.callStillLive(event.sid)) return
        // `event.from` is the responder's full JID (XEP-0353 §0.6); the
        // initiator attribute names our own resource (XEP-0166 §7.1).
        val ourJid = ownFullJid()
        if (ourJid == null) {
            host.failCall(event.sid, "call session initiate failed: no bound resource")
            return
        }
        if (connection.sessionInitiate(event.from, ourJid, event.sid, prev.media)) {
            host.scheduleSessionAcceptTimeout(event.from, event.sid)
        } else {
            host.failCall(event.sid, "call session initiate failed")
        }
    }

    private suspend fun sessionInitiateSideEffect(
        event: WaddleCallEvent,
        kind: WaddleCallEventKind.SessionInitiate,
        prev: CallState,
    ) {
        if (prev !is CallState.Incoming || prev.sid != event.sid) return
        val connection = prev.connection ?: return
        // Same stale-effect guard as the proceed path: a concurrent
        // hang-up already terminated this sid on the wire.
        if (!host.callStillLive(event.sid)) return
        // The responder confirms the Jingle session per XEP-0166 §6.2 —
        // without this the CALLER never gets a populated transport
        // rewrite and never joins the LiveKit room.
        val ourJid = ownFullJid()
        if (ourJid == null) {
            host.failCall(event.sid, "call session accept failed: no bound resource")
            return
        }
        if (!connection.sessionAccept(event.from, ourJid, event.sid, kind.media)) {
            host.failCall(event.sid, "call session accept failed")
        }
    }
}

/** Named call-slot operations used by [CallRemoteEffects], without exposing mutable state. */
internal interface CallRemoteEffectsHost {
    suspend fun handleProposeSideEffect(
        event: WaddleCallEvent,
        kind: WaddleCallEventKind.Propose,
        prev: CallState,
        connection: CallConnection?,
    )

    fun callStillLive(sid: String): Boolean
    fun failCall(sid: String, message: String)
    fun scheduleSessionAcceptTimeout(peerFullJid: String, sid: String)
}

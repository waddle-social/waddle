package social.waddle.android.client.calls

import kotlinx.coroutines.CancellationException
import social.waddle.android.client.VerbResult
import social.waddle.android.client.session.ActiveSession
import social.waddle.client.ffi.WaddleCallMedia
import social.waddle.client.ffi.WaddleCallSessionTerminateOutcome
import social.waddle.client.ffi.WaddleExternalService
import social.waddle.client.ffi.WaddleJingleReason

/**
 * The call wire-verb surface the store and side-effect handler drive —
 * the Kotlin twin of the web's `CallWireSender` (outbound.ts), typed
 * against the FFI records. Every send returns whether the stanza was
 * accepted by the live session; failures surface as `false` (the FFI
 * already emitted the diagnostic through the event listener).
 */
internal interface CallSignaling {
    suspend fun propose(peerBareJid: String, sid: String, media: WaddleCallMedia): Boolean
    suspend fun ringing(peerBareJid: String, sid: String): Boolean
    suspend fun proceed(peerFullJid: String, sid: String): Boolean
    suspend fun reject(peerFullJid: String, sid: String): Boolean
    suspend fun rejectTieBreak(peerFullJid: String, sid: String): Boolean
    suspend fun retract(peerBareJid: String, sid: String): Boolean
    suspend fun retractTieBreak(peerFullJid: String, sid: String): Boolean
    suspend fun finish(peerFullJid: String, sid: String): Boolean

    /**
     * `<finish/>` with an explicit reason — the conformant verb for a
     * responder abandoning a session it already answered with
     * `<proceed/>` (a late `<reject/>` would be a contradictory double
     * answer the peer's devices ignore).
     */
    suspend fun finishWithReason(peerFullJid: String, sid: String, reason: WaddleJingleReason): Boolean
    suspend fun finishMigrated(peerFullJid: String, oldSid: String, newSid: String): Boolean

    suspend fun sessionInitiate(
        peerFullJid: String,
        initiatorFullJid: String,
        sid: String,
        media: WaddleCallMedia,
    ): Boolean

    suspend fun sessionAccept(
        peerFullJid: String,
        responderFullJid: String,
        sid: String,
        media: WaddleCallMedia,
    ): Boolean

    suspend fun sessionTerminate(peerFullJid: String, sid: String, reason: WaddleJingleReason?): Boolean

    suspend fun sessionTerminateWithOutcome(
        peerFullJid: String,
        sid: String,
        reason: WaddleJingleReason?,
    ): WaddleCallSessionTerminateOutcome

    /** XEP-0215 TURN/STUN fetch; `null` when no session is live or the query failed. */
    suspend fun fetchExternalServices(): List<WaddleExternalService>?
}

/** Production [CallSignaling] forwarding to the live attempt's FFI client. */
internal class ClientCallSignaling(
    private val activeSession: ActiveSession,
) : CallSignaling {
    override suspend fun propose(peerBareJid: String, sid: String, media: WaddleCallMedia): Boolean =
        verb { it.sendCallPropose(peerBareJid, sid, media.audio, media.video) }

    override suspend fun ringing(peerBareJid: String, sid: String): Boolean =
        verb { it.sendCallRinging(peerBareJid, sid) }

    override suspend fun proceed(peerFullJid: String, sid: String): Boolean =
        verb { it.sendCallProceed(peerFullJid, sid) }

    override suspend fun reject(peerFullJid: String, sid: String): Boolean =
        verb { it.sendCallReject(peerFullJid, sid) }

    override suspend fun rejectTieBreak(peerFullJid: String, sid: String): Boolean =
        verb { it.sendCallRejectTieBreak(peerFullJid, sid) }

    override suspend fun retract(peerBareJid: String, sid: String): Boolean =
        verb { it.sendCallRetract(peerBareJid, sid) }

    override suspend fun retractTieBreak(peerFullJid: String, sid: String): Boolean =
        verb { it.sendCallRetractTieBreak(peerFullJid, sid) }

    override suspend fun finish(peerFullJid: String, sid: String): Boolean =
        verb { it.sendCallFinish(peerFullJid, sid) }

    override suspend fun finishWithReason(
        peerFullJid: String,
        sid: String,
        reason: WaddleJingleReason,
    ): Boolean = verb { it.sendCallFinishWithReason(peerFullJid, sid, reason) }

    override suspend fun finishMigrated(peerFullJid: String, oldSid: String, newSid: String): Boolean =
        verb { it.sendCallFinishMigrated(peerFullJid, oldSid, newSid) }

    override suspend fun sessionInitiate(
        peerFullJid: String,
        initiatorFullJid: String,
        sid: String,
        media: WaddleCallMedia,
    ): Boolean = verb {
        it.sendCallSessionInitiate(peerFullJid, initiatorFullJid, sid, media.audio, media.video)
    }

    override suspend fun sessionAccept(
        peerFullJid: String,
        responderFullJid: String,
        sid: String,
        media: WaddleCallMedia,
    ): Boolean = verb {
        it.sendCallSessionAccept(peerFullJid, responderFullJid, sid, media.audio, media.video)
    }

    override suspend fun sessionTerminate(
        peerFullJid: String,
        sid: String,
        reason: WaddleJingleReason?,
    ): Boolean = verb { it.sendCallSessionTerminate(peerFullJid, sid, reason) }

    override suspend fun sessionTerminateWithOutcome(
        peerFullJid: String,
        sid: String,
        reason: WaddleJingleReason?,
    ): WaddleCallSessionTerminateOutcome {
        val client = activeSession.client
            ?: return WaddleCallSessionTerminateOutcome.ERROR
        return try {
            client.sendCallSessionTerminateWithOutcome(peerFullJid, sid, reason)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            WaddleCallSessionTerminateOutcome.ERROR
        }
    }

    override suspend fun fetchExternalServices(): List<WaddleExternalService>? =
        activeSession.fetch { it.fetchExternalServices() }

    private suspend fun verb(
        op: suspend (social.waddle.client.ffi.WaddleClientInterface) -> Boolean,
    ): Boolean = activeSession.verbCall(op) == VerbResult.Ok
}

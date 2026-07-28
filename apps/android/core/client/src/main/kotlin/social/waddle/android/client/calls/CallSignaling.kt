package social.waddle.android.client.calls

import kotlinx.coroutines.CancellationException
import social.waddle.android.client.session.ActiveSession
import social.waddle.client.ffi.WaddleCallMedia
import social.waddle.client.ffi.WaddleCallSessionTerminateOutcome
import social.waddle.client.ffi.WaddleClientInterface
import social.waddle.client.ffi.WaddleExternalService
import social.waddle.client.ffi.WaddleInCallPresenceFlags
import social.waddle.client.ffi.WaddleJingleReason

/**
 * One XEP-0272 `<muji/>` presence publication, bundling the FFI's
 * positional fields into a named record at the signaling boundary.
 */
internal data class MujiPresenceUpdate(
    val roomJid: String,
    val nick: String,
    val active: Boolean,
    val preparing: Boolean,
    val video: Boolean,
    val flags: WaddleInCallPresenceFlags,
)

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

    /**
     * XEP-0272 Muji-bearing Jingle `session-initiate` to the SFU mixer
     * (`calls.<domain>`). Resolves the EMPTY IQ-result ack only; the
     * session-accept arrives later as a routed call event. The FFI
     * throws on failure — surfaced here as `false`.
     */
    suspend fun mujiSessionInitiate(
        roomJid: String,
        initiatorFullJid: String,
        sid: String,
        video: Boolean,
    ): Boolean

    /**
     * XEP-0272 §Leaving mixer half: Jingle `session-terminate` for the
     * group-call attempt. The bare-presence leave marker MUST go out
     * first ([updateMujiPresence] with neither flag).
     */
    suspend fun mujiSessionTerminate(roomJid: String, sid: String): Boolean

    /**
     * Publish this occupant's XEP-0272 `<muji/>` MUC presence:
     * `preparing` for the two-phase setup marker, `active` for the
     * content-declaring join, neither for the leave marker. The
     * update's flags re-stamp BOTH `urn:waddle:in-call:0` markers
     * (presence is last-writer-wins).
     */
    suspend fun updateMujiPresence(update: MujiPresenceUpdate): Boolean

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
    private val clientProvider: () -> WaddleClientInterface?,
) : CallSignaling {
    constructor(activeSession: ActiveSession) : this({ activeSession.client })

    companion object {
        /** A logout-only sender captured before ordinary authority was revoked. */
        fun forRetiredConnection(
            connection: ActiveSession.RetiredCallConnection,
        ): ClientCallSignaling = ClientCallSignaling { connection.client }
    }
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

    override suspend fun mujiSessionInitiate(
        roomJid: String,
        initiatorFullJid: String,
        sid: String,
        video: Boolean,
    ): Boolean = verb {
        it.sendMujiSessionInitiate(roomJid, initiatorFullJid, sid, video)
        true
    }

    override suspend fun mujiSessionTerminate(roomJid: String, sid: String): Boolean = verb {
        it.sendMujiSessionTerminate(roomJid, sid)
        true
    }

    override suspend fun updateMujiPresence(update: MujiPresenceUpdate): Boolean = verb {
        it.updateMujiPresence(
            update.roomJid,
            update.nick,
            update.active,
            update.preparing,
            update.video,
            update.flags,
        )
        true
    }

    override suspend fun sessionTerminateWithOutcome(
        peerFullJid: String,
        sid: String,
        reason: WaddleJingleReason?,
    ): WaddleCallSessionTerminateOutcome {
        val client = clientProvider()
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
        invokeOrNull { it.fetchExternalServices() }

    private suspend fun verb(
        op: suspend (social.waddle.client.ffi.WaddleClientInterface) -> Boolean,
    ): Boolean {
        val client = clientProvider() ?: return false
        return try {
            op(client)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            false
        }
    }

    private suspend fun <T : Any> invokeOrNull(
        op: suspend (WaddleClientInterface) -> T,
    ): T? {
        val client = clientProvider() ?: return null
        return try {
            op(client)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            null
        }
    }
}

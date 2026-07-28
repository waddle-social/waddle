package social.waddle.android.client.calls

import kotlinx.coroutines.CancellationException
import social.waddle.android.client.session.ActiveSession
import social.waddle.client.ffi.WaddleCallMedia
import social.waddle.client.ffi.WaddleCallSessionTerminateOutcome
import social.waddle.client.ffi.WaddleClientInterface
import social.waddle.client.ffi.WaddleExternalService
import social.waddle.client.ffi.WaddleInCallPresenceFlags
import social.waddle.client.ffi.WaddleJingleReason

/** One XEP-0272 `<muji/>` presence publication at the typed signaling boundary. */
internal data class MujiPresenceUpdate(
    val roomJid: String,
    val nick: String,
    val active: Boolean,
    val preparing: Boolean,
    val video: Boolean,
    val flags: WaddleInCallPresenceFlags,
)

/** Factory for a connection pinned to one exact ready account attempt. */
internal interface CallSignaling {
    /**
     * Capture the active attempt before a logical call action changes the
     * slot. The returned object never selects a later replacement client.
     */
    fun captureActiveConnection(): CallConnection?
}

/**
 * Typed, attempt-pinned call transport. Every normal FFI operation goes
 * through `ActiveSession.invokeIfCurrent`; only logout receives the distinct
 * retired implementation below.
 */
/**
 * The logout-only authority for the retired transport.  It cannot begin or
 * advance signaling or publish arbitrary state: its verbs encode only the
 * fixed cleanup shapes needed to leave an existing DM/Muji call.
 */
internal interface LogoutCallTeardown {
    suspend fun retractForLogout(peerBareJid: String, sid: String): Boolean
    suspend fun rejectForLogout(peerFullJid: String, sid: String): Boolean
    suspend fun cancelAcceptingCallForLogout(peerFullJid: String, sid: String): Boolean
    suspend fun terminateCallForLogout(peerFullJid: String, sid: String): WaddleCallSessionTerminateOutcome
    suspend fun finishTerminatedCallForLogout(peerFullJid: String, sid: String): Boolean
    suspend fun leaveMujiForLogout(roomJid: String, nick: String): Boolean
    suspend fun terminateMujiForLogout(roomJid: String, sid: String): Boolean
}

internal interface CallConnection {
    val lease: ActiveSession.OwnerLease?

    suspend fun applyIfCurrent(action: () -> Unit): Boolean
    suspend fun propose(peerBareJid: String, sid: String, media: WaddleCallMedia): Boolean
    suspend fun ringing(peerBareJid: String, sid: String): Boolean
    suspend fun proceed(peerFullJid: String, sid: String): Boolean
    suspend fun reject(peerFullJid: String, sid: String): Boolean
    suspend fun rejectTieBreak(peerFullJid: String, sid: String): Boolean
    suspend fun retract(peerBareJid: String, sid: String): Boolean
    suspend fun retractTieBreak(peerFullJid: String, sid: String): Boolean
    suspend fun finish(peerFullJid: String, sid: String): Boolean
    suspend fun finishWithReason(peerFullJid: String, sid: String, reason: WaddleJingleReason): Boolean
    suspend fun finishMigrated(peerFullJid: String, oldSid: String, newSid: String): Boolean
    suspend fun sessionInitiate(peerFullJid: String, initiatorFullJid: String, sid: String, media: WaddleCallMedia): Boolean
    suspend fun sessionAccept(peerFullJid: String, responderFullJid: String, sid: String, media: WaddleCallMedia): Boolean
    suspend fun sessionTerminate(peerFullJid: String, sid: String, reason: WaddleJingleReason?): Boolean
    suspend fun mujiSessionInitiate(roomJid: String, initiatorFullJid: String, sid: String, video: Boolean): Boolean
    suspend fun mujiSessionTerminate(roomJid: String, sid: String): Boolean
    suspend fun updateMujiPresence(update: MujiPresenceUpdate): Boolean
    suspend fun sessionTerminateWithOutcome(peerFullJid: String, sid: String, reason: WaddleJingleReason?): WaddleCallSessionTerminateOutcome
    suspend fun fetchExternalServices(): List<WaddleExternalService>?
}

/** Two wrappers name the same transport only when they carry the same owner generation. */
internal fun CallConnection?.isSameActiveAttempt(other: CallConnection?): Boolean =
    this?.lease != null && this.lease == other?.lease

/** Production [CallSignaling] factory; it deliberately exposes no unpinned verb. */
internal class ClientCallSignaling(private val activeSession: ActiveSession) : CallSignaling {
    override fun captureActiveConnection(): CallConnection? =
        activeSession.captureOwnerLease()?.let { ActiveCallConnection(activeSession, it) }

    companion object {
        /** Logout-only capability captured before ordinary outbound authority is revoked. */
        fun forRetiredConnection(connection: ActiveSession.RetiredCallConnection): LogoutCallTeardown =
            RetiredCallTeardown(connection)
    }
}

private class ActiveCallConnection(
    private val activeSession: ActiveSession,
    override val lease: ActiveSession.OwnerLease,
) : CallConnection {
    override suspend fun applyIfCurrent(action: () -> Unit): Boolean = activeSession.applyIfCurrent(lease, action)

    override suspend fun propose(peerBareJid: String, sid: String, media: WaddleCallMedia): Boolean =
        verb { it.sendCallPropose(peerBareJid, sid, media.audio, media.video) }
    override suspend fun ringing(peerBareJid: String, sid: String): Boolean = verb { it.sendCallRinging(peerBareJid, sid) }
    override suspend fun proceed(peerFullJid: String, sid: String): Boolean = verb { it.sendCallProceed(peerFullJid, sid) }
    override suspend fun reject(peerFullJid: String, sid: String): Boolean = verb { it.sendCallReject(peerFullJid, sid) }
    override suspend fun rejectTieBreak(peerFullJid: String, sid: String): Boolean = verb { it.sendCallRejectTieBreak(peerFullJid, sid) }
    override suspend fun retract(peerBareJid: String, sid: String): Boolean = verb { it.sendCallRetract(peerBareJid, sid) }
    override suspend fun retractTieBreak(peerFullJid: String, sid: String): Boolean = verb { it.sendCallRetractTieBreak(peerFullJid, sid) }
    override suspend fun finish(peerFullJid: String, sid: String): Boolean = verb { it.sendCallFinish(peerFullJid, sid) }
    override suspend fun finishWithReason(peerFullJid: String, sid: String, reason: WaddleJingleReason): Boolean =
        verb { it.sendCallFinishWithReason(peerFullJid, sid, reason) }
    override suspend fun finishMigrated(peerFullJid: String, oldSid: String, newSid: String): Boolean =
        verb { it.sendCallFinishMigrated(peerFullJid, oldSid, newSid) }
    override suspend fun sessionInitiate(peerFullJid: String, initiatorFullJid: String, sid: String, media: WaddleCallMedia): Boolean =
        verb { it.sendCallSessionInitiate(peerFullJid, initiatorFullJid, sid, media.audio, media.video) }
    override suspend fun sessionAccept(peerFullJid: String, responderFullJid: String, sid: String, media: WaddleCallMedia): Boolean =
        verb { it.sendCallSessionAccept(peerFullJid, responderFullJid, sid, media.audio, media.video) }
    override suspend fun sessionTerminate(peerFullJid: String, sid: String, reason: WaddleJingleReason?): Boolean =
        verb { it.sendCallSessionTerminate(peerFullJid, sid, reason) }
    override suspend fun mujiSessionInitiate(roomJid: String, initiatorFullJid: String, sid: String, video: Boolean): Boolean =
        verb { it.sendMujiSessionInitiate(roomJid, initiatorFullJid, sid, video); true }
    override suspend fun mujiSessionTerminate(roomJid: String, sid: String): Boolean =
        verb { it.sendMujiSessionTerminate(roomJid, sid); true }
    override suspend fun updateMujiPresence(update: MujiPresenceUpdate): Boolean = verb {
        it.updateMujiPresence(update.roomJid, update.nick, update.active, update.preparing, update.video, update.flags)
        true
    }

    override suspend fun sessionTerminateWithOutcome(
        peerFullJid: String, sid: String, reason: WaddleJingleReason?,
    ): WaddleCallSessionTerminateOutcome = invokeOr(WaddleCallSessionTerminateOutcome.ERROR) {
        it.sendCallSessionTerminateWithOutcome(peerFullJid, sid, reason)
    }

    override suspend fun fetchExternalServices(): List<WaddleExternalService>? = invokeOr(null) { it.fetchExternalServices() }

    private suspend fun verb(op: suspend (WaddleClientInterface) -> Boolean): Boolean = invokeOr(false, op)

    private suspend fun <T> invokeOr(fallback: T, op: suspend (WaddleClientInterface) -> T): T = try {
        when (val result = activeSession.invokeIfCurrent(lease, op)) {
            ActiveSession.LeaseInvocation.Stale,
            ActiveSession.LeaseInvocation.NotConnected,
            -> fallback
            is ActiveSession.LeaseInvocation.Completed -> result.value
        }
    } catch (cancellation: CancellationException) {
        throw cancellation
    } catch (_: Throwable) {
        fallback
    }
}

/** Explicitly distinct from [ActiveCallConnection]; only logout may construct it. */
private class RetiredCallTeardown(
    private val connection: ActiveSession.RetiredCallConnection,
) : LogoutCallTeardown {
    override suspend fun retractForLogout(peerBareJid: String, sid: String): Boolean = call { it.sendCallRetract(peerBareJid, sid) }
    override suspend fun rejectForLogout(peerFullJid: String, sid: String): Boolean = call { it.sendCallReject(peerFullJid, sid) }
    override suspend fun cancelAcceptingCallForLogout(peerFullJid: String, sid: String): Boolean =
        call { it.sendCallFinishWithReason(peerFullJid, sid, WaddleJingleReason.CANCEL) }
    override suspend fun terminateCallForLogout(peerFullJid: String, sid: String): WaddleCallSessionTerminateOutcome =
        invokeOr(WaddleCallSessionTerminateOutcome.ERROR) {
            it.sendCallSessionTerminateWithOutcome(peerFullJid, sid, WaddleJingleReason.SUCCESS)
        }
    override suspend fun finishTerminatedCallForLogout(peerFullJid: String, sid: String): Boolean =
        call { it.sendCallFinish(peerFullJid, sid) }
    override suspend fun leaveMujiForLogout(roomJid: String, nick: String): Boolean = call {
        it.updateMujiPresence(
            roomJid, nick, active = false, preparing = false, video = false,
            flags = WaddleInCallPresenceFlags(handRaised = false, muted = false),
        )
        true
    }
    override suspend fun terminateMujiForLogout(roomJid: String, sid: String): Boolean =
        call { it.sendMujiSessionTerminate(roomJid, sid); true }
    private suspend fun call(op: suspend (WaddleClientInterface) -> Boolean): Boolean = invokeOr(false, op)
    private suspend fun <T> invokeOr(fallback: T, op: suspend (WaddleClientInterface) -> T): T = try {
        op(connection.client)
    } catch (cancellation: CancellationException) {
        throw cancellation
    } catch (_: Throwable) {
        fallback
    }
}

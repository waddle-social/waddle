package social.waddle.android.client

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.delay
import social.waddle.android.client.OutboundQueue.ResumeTransitionResult
import social.waddle.android.client.prefs.DeliveryAttemptTransition
import social.waddle.android.client.session.ActiveSession
import social.waddle.android.client.session.ResumePersistence
import java.util.logging.Level
import java.util.logging.Logger

internal sealed interface RotationJournalOutcome {
    data class Accepted(
        val smVersion: Long,
    ) : RotationJournalOutcome

    data object Rejected : RotationJournalOutcome
}

/**
 * Stateless durable phase operations. Lifecycle state publication remains in
 * [OutboundLifecycleCoordinator], while this class owns only ordered calls to
 * the journal, resume persistence, drain worker, and active-session projection.
 */
internal class OutboundLifecyclePhaseOperations(
    private val activeSession: ActiveSession,
    private val journal: OutboundQueue,
    private val phaseObserver: OutboundLifecyclePhaseObserver,
    private val resume: ResumePersistence,
) {
    suspend fun journalActivation(
        lifecycle: SessionLifecycleRef,
    ): OutboundQueue.AttemptBootstrap {
        phaseObserver.after(OutboundLifecyclePhase.ATTEMPT_JOURNALING)
        val bootstrap = journal.beginAttempt(lifecycle.ownerBareJid)
        phaseObserver.after(OutboundLifecyclePhase.ATTEMPT_JOURNALED)
        return bootstrap
    }

    suspend fun publishActivation(
        workers: OwnerWorkers,
        lifecycle: SessionLifecycleRef,
        handle: ConnectionAttemptHandle,
        bootstrap: OutboundQueue.AttemptBootstrap,
    ): ActiveSession.Attempt {
        resume.registerAttempt(bootstrap.attempt, bootstrap.smVersion)
        phaseObserver.after(OutboundLifecyclePhase.RESUME_REGISTERED)
        check(workers.drain.bind(handle, bootstrap.attempt)) {
            "outbound drain worker rejected attempt binding"
        }
        phaseObserver.after(OutboundLifecyclePhase.DRAIN_BOUND)
        val active = activeSession.beginAttempt(bootstrap.attempt)
        phaseObserver.after(OutboundLifecyclePhase.ACTIVE_SESSION_PUBLISHED)
        return active
    }

    suspend fun journalRotation(
        transition: DeliveryAttemptTransition,
        affectedStanzaIds: Set<String>,
    ): RotationJournalOutcome {
        val result = retryResumeTransition(transition, affectedStanzaIds)
        val smVersion = when (result) {
            is ResumeTransitionResult.AlreadyCommitted -> result.smVersion
            is ResumeTransitionResult.Committed -> result.smVersion
            is ResumeTransitionResult.AffectedSetMismatch,
            ResumeTransitionResult.InvalidTransition,
            ResumeTransitionResult.ReceiptCapacityExhausted,
            ResumeTransitionResult.ReceiptConflict,
            ResumeTransitionResult.StaleAttempt,
            -> return RotationJournalOutcome.Rejected
        }
        phaseObserver.after(OutboundLifecyclePhase.ROTATION_JOURNALED)
        return RotationJournalOutcome.Accepted(smVersion)
    }

    suspend fun publishRotation(
        workers: OwnerWorkers,
        lifecycle: SessionLifecycleRef,
        handle: ConnectionAttemptHandle,
        transition: DeliveryAttemptTransition,
        smVersion: Long,
    ): Boolean {
        if (!resume.acceptResumeTransition(transition, smVersion)) return false
        phaseObserver.after(OutboundLifecyclePhase.ROTATION_RESUME_REGISTERED)
        if (!workers.drain.bind(handle, transition.fresh)) return false
        phaseObserver.after(OutboundLifecyclePhase.ROTATION_DRAIN_BOUND)
        if (!activeSession.acceptResumeTransition(transition)) return false
        phaseObserver.after(OutboundLifecyclePhase.ROTATION_ACTIVE_SESSION_PUBLISHED)
        return true
    }

    suspend fun attemptPublished() {
        phaseObserver.after(OutboundLifecyclePhase.ATTEMPT_PUBLISHED)
    }

    suspend fun rotationPublished() {
        phaseObserver.after(OutboundLifecyclePhase.ROTATION_PUBLISHED)
    }

    private suspend fun retryResumeTransition(
        transition: DeliveryAttemptTransition,
        affectedStanzaIds: Set<String>,
    ): ResumeTransitionResult {
        var retryIndex = 0
        while (true) {
            try {
                return journal.rotateAfterResumeFailure(transition, affectedStanzaIds)
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (failure: Throwable) {
                LOGGER.log(Level.WARNING, "resume transition commit failed; retrying", failure)
                delay(RETRY_DELAYS_MILLIS[retryIndex.coerceAtMost(RETRY_DELAYS_MILLIS.lastIndex)])
                if (retryIndex < RETRY_DELAYS_MILLIS.lastIndex) retryIndex += 1
            }
        }
    }

    private companion object {
        val LOGGER: Logger =
            Logger.getLogger(OutboundLifecyclePhaseOperations::class.java.name)
        val RETRY_DELAYS_MILLIS =
            longArrayOf(250L, 500L, 1_000L, 2_000L, 5_000L)
    }
}

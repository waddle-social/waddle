package social.waddle.android.client

import java.io.IOException
import java.util.logging.Level
import java.util.logging.Logger
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.delay
import kotlinx.coroutines.withContext
import kotlinx.serialization.SerializationException
import social.waddle.android.client.prefs.DeliveryJournalDecodeException
import social.waddle.android.client.prefs.DeliveryOwnerBareJid
import social.waddle.android.client.prefs.LifecycleGeneration
import social.waddle.android.client.prefs.ProcessEpoch
import social.waddle.android.client.prefs.TerminalClaimId
import social.waddle.android.client.prefs.TerminalReceiptClaimState
import social.waddle.android.client.prefs.TerminalReceiptClaimant
import social.waddle.android.client.prefs.TerminalReceiptEffect
import social.waddle.android.client.prefs.TerminalReceiptWorkerKind
import social.waddle.android.client.prefs.WorkerGeneration as ReceiptWorkerGeneration

/** Applies one durable terminal receipt and owns its exact claim cleanup. */
internal class TerminalReceiptProcessor(
    private val journal: DeliveryJournalStore,
    private val dispatchEvent: (XmppEvent) -> Unit,
    private val processEpoch: ProcessEpoch,
    private val ownership: WorkerOwnership,
) {
    @Volatile private var unresolvedCleanup: TerminalReceiptCleanupException? = null

    suspend fun drain() {
        val owner = DeliveryOwnerBareJid(ownership.lifecycle.ownerBareJid)
        try {
            when (val discovery = retry(TerminalReceiptPersistenceOperation.DISCOVERY, owner, null) {
                journal.discoverTerminalReceipt(owner)
            }) {
                is TerminalReceiptDiscovery.Pending -> claimAndApply(owner, discovery)
                is TerminalReceiptDiscovery.Corrupt -> fail(TerminalReceiptApplicationFailure.DiscoveryCorrupt(owner, discovery.reason))
                is TerminalReceiptDiscovery.OwnerFenced -> fail(TerminalReceiptApplicationFailure.DiscoveryOwnerFenced(discovery.requested, discovery.actual))
                TerminalReceiptDiscovery.None, is TerminalReceiptDiscovery.AlreadyAcknowledged -> Unit
            }
        } catch (failure: DeliveryJournalDecodeException) {
            fail(TerminalReceiptApplicationFailure.DiscoveryCorrupt(owner, TerminalReceiptCorruption.PERSISTED_DECODE_FAILURE), failure)
        }
    }

    suspend fun recoverUnresolvedCleanup(): TerminalReceiptRecoveryCleanupResult {
        val pending = unresolvedCleanup ?: return TerminalReceiptRecoveryCleanupResult.NoPendingLease
        return when (val recovered = release(pending.evidence.lease)) {
            TerminalReceiptCleanupResult.Released -> {
                unresolvedCleanup = null
                TerminalReceiptRecoveryCleanupResult.Released
            }
            is TerminalReceiptCleanupResult.Unresolved -> TerminalReceiptRecoveryCleanupResult.Unresolved(recovered.evidence)
        }
    }

    fun failureOf(failure: Throwable): TerminalReceiptFailureExtraction {
        (failure as? TerminalReceiptApplicationException)?.let { return TerminalReceiptFailureExtraction.Found(it.failure) }
        val cleanup = failure.suppressed.filterIsInstance<TerminalReceiptCleanupException>().lastOrNull()
        return cleanup?.let { TerminalReceiptFailureExtraction.Found(TerminalReceiptApplicationFailure.CleanupUnresolved(it.evidence)) }
            ?: TerminalReceiptFailureExtraction.None
    }

    private suspend fun claimAndApply(owner: DeliveryOwnerBareJid, discovery: TerminalReceiptDiscovery.Pending) {
        val claim = TerminalReceiptClaimState.Claimed(
            TerminalClaimId.random(), ownership.claimant(), processEpoch,
        )
        when (val claimed = retry(TerminalReceiptPersistenceOperation.CLAIM, owner, discovery.ref) {
            journal.claimTerminalReceipt(TerminalReceiptClaimRequest(discovery.ref, claim))
        }) {
            is TerminalReceiptClaimResult.Claimed -> apply(claimed)
            is TerminalReceiptClaimResult.AlreadyAcknowledged -> Unit
            is TerminalReceiptClaimResult.Busy -> fail(TerminalReceiptApplicationFailure.ClaimBusy(claimed))
            is TerminalReceiptClaimResult.ReceiptMissing -> fail(TerminalReceiptApplicationFailure.ClaimMissing(claimed))
            is TerminalReceiptClaimResult.ReceiptReplaced -> fail(TerminalReceiptApplicationFailure.ClaimReplaced(claimed))
            is TerminalReceiptClaimResult.OwnerFenced -> fail(TerminalReceiptApplicationFailure.ClaimOwnerFenced(claimed))
            is TerminalReceiptClaimResult.Corrupt -> fail(TerminalReceiptApplicationFailure.ClaimCorrupt(claimed))
        }
    }

    private suspend fun apply(claimed: TerminalReceiptClaimResult.Claimed) {
        try {
            claimed.effects.forEach(::dispatch)
            when (val result = retry(TerminalReceiptPersistenceOperation.ACKNOWLEDGE, claimed.lease.ref.owner, claimed.lease.ref) {
                journal.acknowledgeTerminalReceipt(claimed.lease)
            }) {
                is TerminalReceiptAcknowledgeResult.Acknowledged, is TerminalReceiptAcknowledgeResult.AlreadyAcknowledged -> Unit
                is TerminalReceiptAcknowledgeResult.LeaseMismatch -> fail(TerminalReceiptApplicationFailure.AcknowledgeLeaseMismatch(result))
                is TerminalReceiptAcknowledgeResult.ReceiptMissing -> fail(TerminalReceiptApplicationFailure.AcknowledgeMissing(result))
                is TerminalReceiptAcknowledgeResult.ReceiptReplaced -> fail(TerminalReceiptApplicationFailure.AcknowledgeReplaced(result))
                is TerminalReceiptAcknowledgeResult.Corrupt -> fail(TerminalReceiptApplicationFailure.AcknowledgeCorrupt(result))
            }
        } catch (failure: Throwable) {
            preserve(failure, claimed.lease)
            throw failure
        }
    }

    private suspend fun preserve(primary: Throwable, lease: TerminalReceiptLease) {
        try {
            val cleanup = release(lease) as? TerminalReceiptCleanupResult.Unresolved
            if (cleanup != null) {
                TerminalReceiptCleanupException(cleanup.evidence).also { unresolvedCleanup = it; primary.addSuppressed(it) }
            }
        } catch (failure: TerminalReceiptCleanupException) {
            unresolvedCleanup = failure
            primary.addSuppressed(failure)
        }
    }

    private suspend fun release(lease: TerminalReceiptLease): TerminalReceiptCleanupResult = withContext(NonCancellable) {
        var attempts = 0
        while (true) {
            try {
                attempts += 1
                return@withContext when (val result = journal.releaseTerminalReceipt(lease)) {
                    is TerminalReceiptReleaseResult.Released, is TerminalReceiptReleaseResult.AlreadyReleased, is TerminalReceiptReleaseResult.AlreadyAcknowledged -> TerminalReceiptCleanupResult.Released
                    is TerminalReceiptReleaseResult.LeaseMismatch -> unresolved(lease, TerminalReceiptCleanupReason.LeaseMismatch(result.current), attempts)
                    is TerminalReceiptReleaseResult.ReceiptMissing -> unresolved(lease, TerminalReceiptCleanupReason.ReceiptMissing, attempts)
                    is TerminalReceiptReleaseResult.ReceiptReplaced -> unresolved(lease, TerminalReceiptCleanupReason.ReceiptReplaced(result.actual), attempts)
                    is TerminalReceiptReleaseResult.Corrupt -> unresolved(lease, TerminalReceiptCleanupReason.Corrupt(result.reason), attempts)
                }
            } catch (failure: Throwable) {
                if (attempts == MAX_ATTEMPTS) throw TerminalReceiptCleanupException(
                    TerminalReceiptCleanupEvidence(lease, attempts, TerminalReceiptCleanupReason.Persistence(category(failure))), failure,
                )
                delay(RETRY_DELAYS[attempts - 1])
            }
        }
        error("unreachable receipt cleanup retry exit")
    }

    private suspend fun <T> retry(operation: TerminalReceiptPersistenceOperation, owner: DeliveryOwnerBareJid, receipt: TerminalReceiptRef?, block: suspend () -> T): T {
        var attempts = 0
        while (true) try {
            attempts += 1
            return block()
        } catch (failure: IOException) {
            if (attempts == MAX_ATTEMPTS) fail(TerminalReceiptApplicationFailure.PersistenceExhausted(operation, owner, receipt, attempts), failure)
            delay(RETRY_DELAYS[attempts - 1])
        }
    }

    private fun dispatch(effect: TerminalReceiptEffect) = when (effect) {
        is TerminalReceiptEffect.Acknowledged -> dispatchEvent(XmppEvent.DeliveryAcked(DeliveryOutcomeRef(effect.row.identity, effect.row.source)))
        is TerminalReceiptEffect.Failed -> dispatchEvent(XmppEvent.DeliveryFailed(DeliveryOutcomeRef(effect.row.identity, effect.row.source)))
    }

    private fun unresolved(lease: TerminalReceiptLease, reason: TerminalReceiptCleanupReason, attempts: Int) =
        TerminalReceiptCleanupResult.Unresolved(TerminalReceiptCleanupEvidence(lease, attempts, reason))
    private fun category(failure: Throwable) = when {
        failure is DeliveryJournalDecodeException && generateSequence<Throwable>(failure) { it.cause }.any { it is SerializationException } -> TerminalReceiptCleanupFailureCategory.CODEC_FAILURE
        failure is IOException -> TerminalReceiptCleanupFailureCategory.IO_FAILURE
        failure is CancellationException -> TerminalReceiptCleanupFailureCategory.CANCELLATION
        failure is SerializationException -> TerminalReceiptCleanupFailureCategory.CODEC_FAILURE
        failure is Error -> TerminalReceiptCleanupFailureCategory.ERROR_FAILURE
        failure is IllegalStateException -> TerminalReceiptCleanupFailureCategory.INVARIANT_FAILURE
        else -> TerminalReceiptCleanupFailureCategory.RUNTIME_FAILURE
    }
    private fun fail(failure: TerminalReceiptApplicationFailure, cause: Throwable? = null): Nothing = throw TerminalReceiptApplicationException(failure, cause)
    private fun WorkerOwnership.claimant() = TerminalReceiptClaimant.Worker(LifecycleGeneration(lifecycle.id.value.toString()), TerminalReceiptWorkerKind.DELIVERY_TERMINAL, ReceiptWorkerGeneration(generation.value.toString()))
    private companion object { val RETRY_DELAYS = longArrayOf(250, 500, 1_000, 2_000, 5_000); const val MAX_ATTEMPTS = 6 }
}

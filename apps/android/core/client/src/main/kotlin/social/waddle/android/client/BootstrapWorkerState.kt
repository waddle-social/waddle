package social.waddle.android.client

/**
 * Gate-confined bookkeeping for building one worker pair.
 *
 * This state never fences a lifecycle: a pre-install failure cannot use the
 * normal recovery path because the sibling may not exist yet. The coordinator
 * owns the enclosing lifecycle state and invokes every method while holding
 * its gate.
 */
internal class BootstrapWorkerBookkeeping {
    private var startupLifecycle: SessionLifecycleRef? = null
    private var teardownOwnerships: Set<WorkerOwnership> = emptySet()
    private var workerExit: BootstrapWorkerExitFailure? = null

    fun begin(lifecycle: SessionLifecycleRef) {
        startupLifecycle = lifecycle
        teardownOwnerships = emptySet()
        workerExit = null
    }

    fun reset() {
        startupLifecycle = null
        teardownOwnerships = emptySet()
        workerExit = null
    }

    fun finishStartup(lifecycle: SessionLifecycleRef) {
        if (startupLifecycle == lifecycle) startupLifecycle = null
    }

    /**
     * A second exact exit can supply the primary thrown by an in-flight
     * readiness await. Keep only that bootstrap-pair evidence until start
     * compensates or opens; every other ignored exit is disposed immediately.
     */
    fun retainsIgnoredExitEvidence(lifecycle: SessionLifecycleRef, workers: OwnerWorkers): Boolean =
        startupLifecycle == lifecycle && workers.isInstalled()

    fun installIfCurrent(
        state: OutboundLifecycleState,
        lifecycle: SessionLifecycleRef,
        workers: OwnerWorkers,
        terminal: DeliveryTerminalWorker.Run,
        drain: OutboundDrainWorker.Run,
    ): Boolean {
        if (state != OutboundLifecycleState.Bootstrapping(lifecycle) || workerExit != null) return false
        workers.install(terminal, drain)
        return true
    }

    fun markPartialTeardown(
        state: OutboundLifecycleState,
        workers: OwnerWorkers?,
        ownership: WorkerOwnership,
    ) {
        if (
            state is OutboundLifecycleState.Bootstrapping &&
            workers?.terminalOwnership == ownership
        ) {
            teardownOwnerships = setOf(ownership)
        }
    }

    fun markCompensation(
        state: OutboundLifecycleState,
        workers: OwnerWorkers?,
    ) {
        if (state is OutboundLifecycleState.Bootstrapping && workers?.terminalOrNull() != null) {
            teardownOwnerships = setOf(workers.terminalOwnership, workers.drainOwnership)
        }
    }

    fun failureFor(lifecycle: SessionLifecycleRef): BootstrapWorkerExitFailure? =
        workerExit?.takeIf { it.exit.lifecycle == lifecycle }

    fun isExpectedTeardown(exit: WorkerExit): Boolean =
        exit.ownership() in teardownOwnerships && exit.reason is WorkerExitReason.RequestedStop

    /** Records only the first exact exit for this not-yet-installed pair. */
    fun recordPreInstallExit(
        state: OutboundLifecycleState,
        workers: OwnerWorkers?,
        exit: WorkerExit,
    ): BootstrapExitDisposition {
        val exactBootstrapExit =
            workers != null &&
                state is OutboundLifecycleState.Bootstrapping &&
                workers.lifecycle == exit.lifecycle &&
                workers.owns(exit.ownership())
        if (!exactBootstrapExit) return BootstrapExitDisposition.NotBootstrap
        if (isExpectedTeardown(exit)) return BootstrapExitDisposition.ExpectedTeardown
        if (workerExit == null) {
            workers.recordExactExit(exit)
            workerExit = BootstrapWorkerExitFailure(exit)
            return BootstrapExitDisposition.RecordedFailure
        }
        return if (workerExit?.exit?.ownership() == exit.ownership()) {
            BootstrapExitDisposition.DuplicateFailure
        } else {
            BootstrapExitDisposition.SecondaryFailure
        }
    }
}

internal enum class BootstrapExitDisposition {
    NotBootstrap,
    ExpectedTeardown,
    RecordedFailure,
    DuplicateFailure,
    SecondaryFailure,
}

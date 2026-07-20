package social.waddle.android.client

import java.util.UUID

@JvmInline
internal value class SessionLifecycleId private constructor(val value: UUID) {
    companion object { fun random(): SessionLifecycleId = SessionLifecycleId(UUID.randomUUID()) }
}

internal data class SessionLifecycleRef(val ownerBareJid: String, val id: SessionLifecycleId) {
    companion object { fun create(ownerBareJid: String) = SessionLifecycleRef(ownerBareJid, SessionLifecycleId.random()) }
}

@JvmInline
internal value class ConnectionAttemptHandle private constructor(val value: UUID) {
    companion object { fun random(): ConnectionAttemptHandle = ConnectionAttemptHandle(UUID.randomUUID()) }
}

internal sealed interface LifecycleStartResult {
    val lifecycle: SessionLifecycleRef
    data class Started(override val lifecycle: SessionLifecycleRef) : LifecycleStartResult
    data class Failed(override val lifecycle: SessionLifecycleRef, val cause: LifecycleStartFailure) : LifecycleStartResult
}

internal enum class LifecycleStartFailure { WORKER_CONSTRUCTION_FAILED, WORKER_READINESS_FAILED }
internal class LifecycleStartException(val result: LifecycleStartResult.Failed) : IllegalStateException("outbound worker startup failed: ${result.cause}")

internal sealed interface BeginShutdownDecision {
    data class Begun(val lifecycle: SessionLifecycleRef) : BeginShutdownDecision
    data class AlreadyClosing(val lifecycle: SessionLifecycleRef) : BeginShutdownDecision
    data class WorkerFenced(val lifecycle: SessionLifecycleRef, val cause: LifecycleFenceCause) : BeginShutdownDecision
    data class Stale(val requested: SessionLifecycleRef, val actual: SessionLifecycleRef?) : BeginShutdownDecision
}

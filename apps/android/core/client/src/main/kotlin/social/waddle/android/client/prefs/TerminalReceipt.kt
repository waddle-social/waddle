package social.waddle.android.client.prefs

import java.util.UUID
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

@Serializable
@JvmInline
value class DeliveryOwnerBareJid(val value: String) {
    init {
        require(value.isNotBlank()) { "delivery receipt owner must not be blank" }
    }
}

@Serializable
@JvmInline
value class TerminalReceiptId(val value: String) {
    init {
        requireUuid(value, "terminal receipt id")
    }

    companion object {
        fun random(): TerminalReceiptId = TerminalReceiptId(UUID.randomUUID().toString())
    }
}

@Serializable
@JvmInline
value class TerminalClaimId(val value: String) {
    init {
        requireUuid(value, "terminal claim id")
    }

    companion object {
        fun random(): TerminalClaimId = TerminalClaimId(UUID.randomUUID().toString())
    }
}

@Serializable
@JvmInline
value class ProcessEpoch(val value: String) {
    init {
        requireUuid(value, "process epoch")
    }

    companion object {
        fun random(): ProcessEpoch = ProcessEpoch(UUID.randomUUID().toString())
    }
}

@Serializable
@JvmInline
value class LifecycleGeneration(val value: String) {
    init {
        requireUuid(value, "lifecycle generation")
    }
}

@Serializable
@JvmInline
value class WorkerGeneration(val value: String) {
    init {
        requireUuid(value, "worker generation")
    }
}

@Serializable
@JvmInline
value class FinalizerGeneration(val value: String) {
    init {
        requireUuid(value, "finalizer generation")
    }
}

@Serializable
sealed interface TerminalReceiptClaimant {
    @Serializable
    @SerialName("worker")
    data class Worker(
        val lifecycleGeneration: LifecycleGeneration,
        val kind: TerminalReceiptWorkerKind,
        val workerGeneration: WorkerGeneration,
    ) : TerminalReceiptClaimant

    @Serializable
    @SerialName("finalizer")
    data class Finalizer(
        val lifecycleGeneration: LifecycleGeneration,
        val finalizerGeneration: FinalizerGeneration,
    ) : TerminalReceiptClaimant

    @Serializable
    @SerialName("bootstrap-process")
    data object BootstrapProcess : TerminalReceiptClaimant
}

@Serializable
enum class TerminalReceiptWorkerKind {
    OUTBOUND_DRAIN,
    DELIVERY_TERMINAL,
}

@Serializable
sealed interface TerminalReceiptClaimState {
    @Serializable
    @SerialName("unclaimed")
    data object Unclaimed : TerminalReceiptClaimState

    @Serializable
    @SerialName("claimed")
    data class Claimed(
        val id: TerminalClaimId,
        val claimant: TerminalReceiptClaimant,
        val processEpoch: ProcessEpoch,
    ) : TerminalReceiptClaimState
}

@Serializable
sealed interface TerminalReceiptEffect {
    val callback: DeliveryCallbackRef
    val row: QueuedOutboundMessage

    @Serializable
    @SerialName("acknowledged")
    data class Acknowledged(
        override val callback: DeliveryCallbackRef,
        override val row: QueuedOutboundMessage,
    ) : TerminalReceiptEffect {
        init {
            require(callback.row == row.identity) { "terminal receipt callback must match its row" }
        }
    }

    @Serializable
    @SerialName("failed")
    data class Failed(
        override val callback: DeliveryCallbackRef,
        override val row: QueuedOutboundMessage,
    ) : TerminalReceiptEffect {
        init {
            require(callback.row == row.identity) { "terminal receipt callback must match its row" }
        }
    }
}

@Serializable
sealed interface TerminalReceiptState {
    @Serializable
    @SerialName("pending")
    data class Pending(
        val claim: TerminalReceiptClaimState,
        val effects: List<TerminalReceiptEffect>,
    ) : TerminalReceiptState {
        init {
            require(effects.isNotEmpty()) { "a pending terminal receipt requires effects" }
            require(effects.map { it.callback }.toSet().size == effects.size) {
                "a pending terminal receipt cannot repeat callbacks"
            }
            require(effects.map { it.row.identity }.toSet().size == effects.size) {
                "a pending terminal receipt cannot repeat row identities"
            }
        }
    }

    @Serializable
    @SerialName("acknowledged")
    data object Acknowledged : TerminalReceiptState
}

@Serializable
data class TerminalReceipt(
    val owner: DeliveryOwnerBareJid,
    val attempt: DeliveryAttemptRef,
    val id: TerminalReceiptId,
    val originProcessEpoch: ProcessEpoch,
    val preparedAtMillis: Long,
    val state: TerminalReceiptState,
) {
    init {
        require(owner.value == attempt.ownerBareJid) {
            "terminal receipt owner must match its delivery attempt"
        }
        require(preparedAtMillis >= 0) { "terminal receipt timestamp must be non-negative" }
        if (state is TerminalReceiptState.Pending) {
            require(state.effects.all { effect ->
                effect.row.ownerBareJid == owner.value &&
                    effect.callback.attempt == attempt
            }) { "terminal receipt effects must match the receipt owner and attempt" }
        }
    }
}

private fun requireUuid(value: String, label: String) {
    val parsed = try {
        UUID.fromString(value)
    } catch (_: IllegalArgumentException) {
        throw IllegalArgumentException("$label must be a UUID")
    }
    require(parsed.toString() == value) { "$label must use canonical lowercase UUID spelling" }
}

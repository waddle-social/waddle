package social.waddle.android.client

import social.waddle.android.client.prefs.DeliveryAttemptRef
import social.waddle.android.client.prefs.DeliverySource
import social.waddle.client.ffi.WaddleClientInterface

internal sealed interface LiveOutboundPurpose { data class MessageSend(val source: DeliverySource) : LiveOutboundPurpose; data object Drain : LiveOutboundPurpose }
internal sealed interface OutboundAdmissionLease {
    val lifecycle: SessionLifecycleRef
    val capability: LifecycleOperationRegistry.Lease
    class OfflineOutbound private constructor(val source: DeliverySource, override val capability: LifecycleOperationRegistry.Lease) : OutboundAdmissionLease {
        override val lifecycle get() = capability.lifecycle; val attempt: DeliveryAttemptRef? get() = capability.attempt
        companion object { internal fun issue(source: DeliverySource, capability: LifecycleOperationRegistry.Lease) = OfflineOutbound(source, capability) }
    }
    class LiveOutbound private constructor(val client: WaddleClientInterface, val purpose: LiveOutboundPurpose, override val capability: LifecycleOperationRegistry.Lease) : OutboundAdmissionLease {
        override val lifecycle get() = capability.lifecycle; val attempt get() = requireNotNull(capability.attempt)
        companion object { internal fun issue(client: WaddleClientInterface, purpose: LiveOutboundPurpose, capability: LifecycleOperationRegistry.Lease): LiveOutbound { requireNotNull(capability.attempt) { "live admission capability requires an attempt" }; return LiveOutbound(client, purpose, capability) } }
    }
    class Terminal private constructor(override val capability: LifecycleOperationRegistry.Lease) : OutboundAdmissionLease {
        override val lifecycle get() = capability.lifecycle; val attempt get() = requireNotNull(capability.attempt)
        companion object { internal fun issue(capability: LifecycleOperationRegistry.Lease): Terminal { requireNotNull(capability.attempt) { "terminal admission capability requires an attempt" }; return Terminal(capability) } }
    }
}
internal sealed interface OutboundAdmissionResult { data class Granted(val lease: OutboundAdmissionLease) : OutboundAdmissionResult; data object OwnerMismatch : OutboundAdmissionResult; data object LifecycleUnavailable : OutboundAdmissionResult }

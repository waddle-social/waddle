package social.waddle.android.client.calls

import social.waddle.client.ffi.WaddleCallEvent
import social.waddle.client.ffi.WaddleCallEventKind

/**
 * Which self-originated carbons may touch the live slot: only sibling-device
 * answered, declined, or ended transitions. All other echoes are local work
 * already performed by this device and must not flap the slot.
 */
internal object CallSelfOriginatedEventPolicy {
    fun shouldTouchCurrentCall(prev: CallState, event: WaddleCallEvent): Boolean {
        val prevSid = prev.sidOrNull ?: return false
        if (prevSid != event.sid) return false
        return when (event.kind) {
            is WaddleCallEventKind.Propose -> false
            is WaddleCallEventKind.Proceed -> true
            is WaddleCallEventKind.Reject -> prev is CallState.Incoming
            is WaddleCallEventKind.SessionInitiate -> prev is CallState.Incoming
            is WaddleCallEventKind.SessionAccept -> prev is CallState.Outgoing
            is WaddleCallEventKind.SessionTerminate -> prev is CallState.Active
            is WaddleCallEventKind.Finish -> prev is CallState.Active
            else -> false
        }
    }
}

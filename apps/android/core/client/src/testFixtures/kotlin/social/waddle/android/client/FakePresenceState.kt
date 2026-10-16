package social.waddle.android.client

import kotlinx.coroutines.CompletableDeferred
import java.util.concurrent.CopyOnWriteArrayList

/**
 * One recorded own-presence send. [joinsBefore] and [sendsBefore] count
 * the room joins and message sends the same client had already served,
 * which places the presence within the ready pipeline.
 */
data class RecordedPresence(
    val status: String?,
    val show: String?,
    val idleSince: String?,
    val joinsBefore: Int,
    val sendsBefore: Int,
)

/**
 * Own-presence fake state for [FakeWaddleClient]: recorded sends in
 * order, plus a stall knob so tests can change the desired presence
 * while a send is in flight. Extracted like [FakeTopologyState] to keep
 * the fake within the LargeClass budget.
 */
class FakePresenceState {
    /** Every own-presence send, recorded on entry, in send order. */
    val calls = CopyOnWriteArrayList<RecordedPresence>()

    /** When set, each send parks on it after being recorded. */
    @Volatile
    var stall: CompletableDeferred<Unit>? = null

    suspend fun send(call: RecordedPresence) {
        calls += call
        stall?.await()
    }
}

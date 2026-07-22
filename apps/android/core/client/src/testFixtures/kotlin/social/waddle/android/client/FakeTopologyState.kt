package social.waddle.android.client

import kotlinx.coroutines.delay
import social.waddle.client.ffi.WaddleTopology

/**
 * Topology-discovery fake state for [FakeWaddleClient]: canned
 * spaces/channels result plus call-count and failure knobs. Extracted
 * so the fake of the full generated FFI interface stays within the
 * LargeClass budget (the [FakeInboxState] pattern).
 */
class FakeTopologyState {
    /** Canned topology served by [discoverTopology]. */
    @Volatile
    var result: WaddleTopology = WaddleTopology(spaces = emptyList(), channels = emptyList())

    /** Number of [discoverTopology] calls served so far. */
    @Volatile
    var calls = 0

    /** When set, [discoverTopology] throws instead of answering. */
    @Volatile
    var failure: Throwable? = null

    /**
     * Virtual-time stall before [discoverTopology] answers — lets
     * tests retire the session while a refresh is parked mid-flight.
     */
    @Volatile
    var delayMillis = 0L

    suspend fun discoverTopology(): WaddleTopology {
        if (delayMillis > 0) delay(delayMillis)
        calls += 1
        failure?.let { throw it }
        return result
    }
}

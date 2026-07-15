package social.waddle.android.client

import kotlin.random.Random

/**
 * Pure backoff calculator mirroring the web `ReconnectScheduler`
 * (`chat/src/lib/xmpp/client-connection.ts`): `min(2000 · 2^attempt,
 * 60_000)` with a 10-attempt budget, plus full jitter (the delay is
 * multiplied by a random factor in `[0.5, 1.0)`) — an Android-side
 * improvement the web loop doesn't need.
 */
class ReconnectPolicy(private val random: Random = Random.Default) {
    /**
     * Delay before retry number `attempt + 1`, or `null` once the
     * attempt budget is exhausted (terminal failure, web #1164
     * semantics). `attempt` starts at 0 and resets on session-ready.
     */
    fun delayMillisFor(attempt: Int): Long? {
        require(attempt >= 0) { "attempt must be >= 0, was $attempt" }
        if (attempt >= MAX_ATTEMPTS) return null
        val base = (BASE_DELAY_MILLIS shl attempt).coerceAtMost(MAX_DELAY_MILLIS)
        return (base * random.nextDouble(JITTER_MIN, JITTER_MAX)).toLong()
    }

    companion object {
        const val MAX_ATTEMPTS = 10
        private const val BASE_DELAY_MILLIS = 2_000L
        private const val MAX_DELAY_MILLIS = 60_000L
        private const val JITTER_MIN = 0.5
        private const val JITTER_MAX = 1.0
    }
}

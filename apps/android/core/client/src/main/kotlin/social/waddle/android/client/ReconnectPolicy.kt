package social.waddle.android.client

import kotlin.random.Random

/**
 * Pure backoff calculator: `min(2000 · 2^attempt, 60_000)` with full
 * jitter (the delay is multiplied by a random factor in `[0.5, 1.0)`).
 *
 * There is no attempt budget. A chat client that parks itself in a
 * terminal "failed" state after a few minutes of outage silently stops
 * receiving messages until someone notices; the Apple client retries
 * forever with a capped delay and so does this one. Recovery is still
 * accelerated by real signals (network return, app foreground, manual
 * retry) that cut the current wait short.
 */
class ReconnectPolicy(private val random: Random = Random.Default) {
    /** Delay before retry number `attempt + 1`; `attempt` resets on session-ready. */
    fun delayMillisFor(attempt: Int): Long {
        require(attempt >= 0) { "attempt must be >= 0, was $attempt" }
        val base = (BASE_DELAY_MILLIS shl attempt.coerceAtMost(MAX_EXPONENT)).coerceAtMost(MAX_DELAY_MILLIS)
        return (base * random.nextDouble(JITTER_MIN, JITTER_MAX)).toLong()
    }

    companion object {
        private const val BASE_DELAY_MILLIS = 2_000L
        private const val MAX_DELAY_MILLIS = 60_000L

        /** 2_000 shl 5 already exceeds the cap; bounding the shift avoids overflow. */
        private const val MAX_EXPONENT = 5
        private const val JITTER_MIN = 0.5
        private const val JITTER_MAX = 1.0
    }
}

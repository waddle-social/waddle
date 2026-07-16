package social.waddle.android.client

import kotlin.random.Random

/** Deterministic Random: every ranged nextDouble returns [value]. */
class PinnedRandom(private val value: Double) : Random() {
    override fun nextBits(bitCount: Int): Int = 0

    override fun nextDouble(from: Double, until: Double): Double {
        require(value >= from && value < until) { "pinned $value outside [$from, $until)" }
        return value
    }
}

package social.waddle.android.feature.home

import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import kotlinx.coroutines.delay

/**
 * [fresh] in the order the user already sees: keys that were in
 * [previous] keep their place, keys new since then follow in [fresh]'s
 * own order, keys that left are dropped. Lists that re-sort on every
 * presence or message make the row under a thumb jump away; this is how
 * Rooms, People and the who-is-here strip hold still while viewed.
 */
fun <T, K> stableOrderOf(previous: List<K>, fresh: List<T>, keyOf: (T) -> K): List<T> {
    if (previous.isEmpty()) return fresh
    val rank = HashMap<K, Int>(previous.size * 2)
    previous.forEachIndexed { index, key -> rank.putIfAbsent(key, index) }
    val (known, arrived) = fresh.partition { keyOf(it) in rank }
    return known.sortedBy { rank.getValue(keyOf(it)) } + arrived
}

/**
 * Holds [sorted] in the order it had when the screen was entered, and
 * re-sorts once the user has been idle for [idleMillis] ([interacting]
 * false: not scrolling, not typing). Entering the screen again starts
 * from the fresh sort, because leaving it drops this state.
 */
@Composable
fun <T, K> rememberStableOrder(
    sorted: List<T>,
    keyOf: (T) -> K,
    interacting: Boolean,
    idleMillis: Long = STABLE_ORDER_IDLE_MILLIS,
): List<T> {
    var frozen by remember { mutableStateOf<List<K>>(emptyList()) }
    var generation by remember { mutableIntStateOf(0) }
    val shown = remember(sorted, frozen) { stableOrderOf(frozen, sorted, keyOf) }
    // Adopt whatever is shown as the order to keep: new arrivals settle
    // at the end and stay there until the next re-sort.
    SideEffect {
        val shownKeys = shown.map(keyOf)
        if (shownKeys != frozen) frozen = shownKeys
    }
    LaunchedEffect(interacting, generation) {
        if (interacting) return@LaunchedEffect
        delay(idleMillis)
        frozen = emptyList()
        generation++
    }
    return shown
}

/** A minute without touching the list: long enough to finish reading a row. */
const val STABLE_ORDER_IDLE_MILLIS = 60_000L

package social.waddle.android.feature.home

import androidx.compose.runtime.Composable
import androidx.compose.runtime.State
import androidx.compose.runtime.produceState
import kotlinx.coroutines.delay
import java.time.Clock
import java.time.ZonedDateTime

/**
 * The wall clock as state, updated on each minute boundary: enough for
 * greetings, quiet lines and "active" kickers, and nothing ticks faster.
 * Screens take the [Clock] as a parameter so tests and goldens pin it.
 */
@Composable
fun rememberNow(clock: Clock): State<ZonedDateTime> = produceState(ZonedDateTime.now(clock), clock) {
    while (true) {
        val millis = clock.millis()
        delay(MINUTE_MILLIS - millis % MINUTE_MILLIS)
        value = ZonedDateTime.now(clock)
    }
}

private const val MINUTE_MILLIS = 60_000L

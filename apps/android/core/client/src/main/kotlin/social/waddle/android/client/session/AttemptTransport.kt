package social.waddle.android.client.session

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.runInterruptible
import kotlinx.coroutines.withTimeoutOrNull
import social.waddle.client.ffi.WaddleClientInterface

internal const val ATTEMPT_TEARDOWN_TIMEOUT_MILLIS = 5_000L

/** Closes an attempt-local FFI transport without extending teardown indefinitely. */
internal suspend fun closeAttemptTransport(client: WaddleClientInterface?): Boolean {
    val closeable = client as? AutoCloseable ?: return true
    return withTimeoutOrNull(ATTEMPT_TEARDOWN_TIMEOUT_MILLIS) {
        runInterruptible(Dispatchers.IO) { closeable.close() }
        true
    } == true
}

package social.waddle.android.client

import kotlinx.coroutines.CancellationException

/**
 * Passthroughs document a never-throw contract, but DataStore writes
 * can raise IOException (disk-full, corruption). Persistence best-
 * effort here: losing a prefs write degrades a convenience (queue,
 * recency), while an escaped throw would crash the caller's scope.
 */
internal suspend fun persistQuietly(write: suspend () -> Unit) {
    try {
        write()
    } catch (cancellation: CancellationException) {
        throw cancellation
    } catch (_: Throwable) {
    }
}

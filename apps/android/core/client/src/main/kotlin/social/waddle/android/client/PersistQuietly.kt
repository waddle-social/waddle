package social.waddle.android.client

import kotlinx.coroutines.CancellationException

/**
 * Passthroughs document a never-throw contract, but DataStore writes
 * can raise IOException (disk-full, corruption). Persistence best-
 * effort here only covers disposable projections such as recency and
 * catch-up cursors. Delivery-journal writes use explicit durable barriers.
 */
internal suspend fun persistQuietly(write: suspend () -> Unit) {
    try {
        write()
    } catch (cancellation: CancellationException) {
        throw cancellation
    } catch (_: Throwable) {
    }
}

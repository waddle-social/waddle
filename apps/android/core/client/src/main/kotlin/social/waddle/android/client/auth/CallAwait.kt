package social.waddle.android.client.auth

import kotlinx.coroutines.suspendCancellableCoroutine
import okhttp3.Call
import okhttp3.Callback
import okhttp3.Response
import java.io.IOException
import kotlin.coroutines.resumeWithException

/** Suspend until the call completes; cancelling the coroutine cancels the call. */
internal suspend fun Call.await(): Response = suspendCancellableCoroutine { continuation ->
    enqueue(
        object : Callback {
            override fun onResponse(call: Call, response: Response) {
                // The onCancellation lambda closes the body when the
                // response arrives exactly as the coroutine is cancelled —
                // a plain resume would leak the connection.
                continuation.resume(response) { _, res, _ -> res.close() }
            }

            override fun onFailure(call: Call, e: IOException) {
                if (continuation.isCancelled) return
                continuation.resumeWithException(e)
            }
        },
    )
    continuation.invokeOnCancellation { runCatching { cancel() } }
}

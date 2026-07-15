package social.waddle.android.client.auth

import java.io.IOException
import java.io.InterruptedIOException
import java.net.ConnectException
import java.net.SocketException

/** Typed failure surface of [WaddleAuthApi], carried inside [Result]. */
sealed class AuthError(message: String, cause: Throwable? = null) : Exception(message, cause) {
    /** Non-2xx HTTP status with the server's `{message|error}` detail. */
    data class Server(val statusCode: Int, val detail: String) :
        AuthError(if (detail.isEmpty()) "Request failed ($statusCode)." else "Request failed ($statusCode): $detail")

    /** Transport-level failure before an HTTP status was received. */
    data class Network(val ioCause: IOException) :
        AuthError("Network request failed: ${ioCause.message ?: ioCause.javaClass.simpleName}", ioCause)

    /** A 2xx body that did not parse into the expected shape. */
    data class InvalidResponse(val parseCause: Throwable?) :
        AuthError("Invalid response from server.", parseCause)
}

/**
 * Transient-vs-fatal classification for device-poll failures, mirroring
 * Apple's `isTransientDeviceAuthPollError`: only "the connection dropped
 * mid-flight" (`networkConnectionLost`) and "timed out" (`timedOut`) are
 * retried on the next poll tick; DNS failures, refused connections, and
 * HTTP errors abort the flow.
 */
fun isTransientDeviceAuthPollError(error: Throwable): Boolean {
    val cause = (error as? AuthError.Network)?.ioCause ?: return false
    return when (cause) {
        // Refused connections are fatal (Apple's cannotConnectToHost is
        // not transient); ConnectException extends SocketException so it
        // must be excluded before the SocketException match.
        is ConnectException -> false
        // Established connection dropped: networkConnectionLost analog.
        is SocketException -> true
        // Socket/call timeouts: timedOut analog. SocketTimeoutException
        // extends InterruptedIOException, as does OkHttp's call timeout.
        is InterruptedIOException -> true
        else -> false
    }
}

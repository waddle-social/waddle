package social.waddle.android.client

import social.waddle.client.ffi.WaddleSaslCondition

/**
 * Whether a typed RFC 6120 SASL failure may reuse the same session
 * automatically.
 */
enum class SaslRetryDisposition {
    RETRY,
    STOP_CREDENTIAL,
    STOP_CONFIGURATION,
    STOP_ABORTED,
    STOP_UNKNOWN,
}

internal fun WaddleSaslCondition.retryDisposition(): SaslRetryDisposition =
    when (this) {
        WaddleSaslCondition.TEMPORARY_AUTH_FAILURE -> SaslRetryDisposition.RETRY
        WaddleSaslCondition.NOT_AUTHORIZED,
        WaddleSaslCondition.ACCOUNT_DISABLED,
        WaddleSaslCondition.CREDENTIALS_EXPIRED,
        WaddleSaslCondition.INVALID_AUTHZID,
        -> SaslRetryDisposition.STOP_CREDENTIAL
        WaddleSaslCondition.INVALID_MECHANISM,
        WaddleSaslCondition.MECHANISM_TOO_WEAK,
        WaddleSaslCondition.ENCRYPTION_REQUIRED,
        WaddleSaslCondition.INCORRECT_ENCODING,
        WaddleSaslCondition.MALFORMED_REQUEST,
        -> SaslRetryDisposition.STOP_CONFIGURATION
        WaddleSaslCondition.ABORTED -> SaslRetryDisposition.STOP_ABORTED
        WaddleSaslCondition.UNKNOWN -> SaslRetryDisposition.STOP_UNKNOWN
    }

/** Connectivity state machine of the XMPP session loop. */
sealed interface ConnectionState {
    /** No session; nothing running. */
    data object Idle : ConnectionState

    /** An attempt is in flight (connect budget running). */
    data object Connecting : ConnectionState

    /** Session bound and ready. */
    data object Ready : ConnectionState

    /** Backoff timer armed before retry number [attempt]. */
    data class Reconnecting(val attempt: Int, val nextDelayMs: Long) : ConnectionState

    /** Network is gone; waiting for connectivity instead of burning attempts. */
    data object Offline : ConnectionState

    /**
     * Terminal for this login/config generation. Only [SaslRetryDisposition.RETRY]
     * may enter the reconnect budget.
     */
    data class AuthenticationStopped(
        val condition: WaddleSaslCondition,
        val disposition: SaslRetryDisposition,
    ) : ConnectionState {
        init {
            require(disposition != SaslRetryDisposition.RETRY) {
                "retryable SASL failures cannot enter a stopped state"
            }
        }
    }

    /** Terminal: the reconnect attempt budget is exhausted. */
    data object Failed : ConnectionState
}

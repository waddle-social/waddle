package social.waddle.android.client

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

    /** Terminal: the server rejected the credentials — re-login required. */
    data object AuthFailed : ConnectionState

    /** Terminal: the reconnect attempt budget is exhausted. */
    data object Failed : ConnectionState
}

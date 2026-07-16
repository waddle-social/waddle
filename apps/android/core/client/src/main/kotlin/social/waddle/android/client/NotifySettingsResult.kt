package social.waddle.android.client

/**
 * Typed outcome of the XEP-0492 set-mode verbs. [VerbResult]'s shape
 * plus one extra signal: [NodeConfigMismatch] (XEP-0060
 * `precondition-not-met` — a pre-existing PEP node with a divergent
 * config) deserves its own user-facing message instead of the generic
 * "action failed" copy.
 */
sealed interface NotifySettingsResult {
    /** The mode was published (or the DM override retracted). */
    data object Ok : NotifySettingsResult

    /** Marker for every non-[Ok] outcome the UI must surface. */
    sealed interface Failure : NotifySettingsResult

    /** The bookmark node exists with an incompatible configuration. */
    data object NodeConfigMismatch : Failure

    /** No live session; nothing was sent. */
    data object NotConnected : Failure

    /** The server refused the publish or the transport broke mid-call. */
    data object Rejected : Failure
}

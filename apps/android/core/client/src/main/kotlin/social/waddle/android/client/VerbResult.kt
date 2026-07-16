package social.waddle.android.client

/** Outcome of a fire-and-check conversation verb. Proportionate to what
 *  the FFI can distinguish today; a Stanza(condition) variant can join
 *  once the FFI surfaces typed stanza errors. */
sealed interface VerbResult {
    /** The verb was carried by a live session and accepted. */
    data object Ok : VerbResult

    sealed interface Failure : VerbResult

    /** No live session existed to carry the verb (offline/reconnecting). */
    data object NotConnected : Failure

    /** A live session carried it and it was refused or the transport broke. */
    data object Rejected : Failure

    /** A local precondition failed (signed out mid-flight, target unresolvable). */
    data object NotReady : Failure
}

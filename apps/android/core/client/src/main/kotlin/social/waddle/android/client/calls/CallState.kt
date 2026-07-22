package social.waddle.android.client.calls

import social.waddle.client.ffi.WaddleCallMedia
import social.waddle.client.ffi.WaddleJingleReason
import social.waddle.client.ffi.WaddleLiveKitJoin

/**
 * Single-slot call lifecycle state, a literal port of the web client's
 * `CallState` (chat/src/lib/calls/types.ts) minus the Muji-only
 * `muc-pending` phase, which arrives with group calls. One call at a
 * time; concurrent calls would need a map keyed by sid.
 */
sealed interface CallState {
    data object Idle : CallState

    /** A remote `<propose/>` is ringing us (XEP-0353 §5.1.1). */
    data class Incoming(
        /** The proposer's FULL JID as stamped by the server (§0.6). */
        val from: String,
        val sid: String,
        val media: WaddleCallMedia,
        /** The local user tapped accept; `<proceed/>` is in flight. */
        val accepting: Boolean = false,
    ) : CallState

    /** Our `<propose/>` is ringing the peer. */
    data class Outgoing(
        /** The callee's bare JID (propose targets the bare JID). */
        val to: String,
        val sid: String,
        val media: WaddleCallMedia,
        /** Our own full JID, the XEP-0166 §7.1 initiator. */
        val initiator: String? = null,
        /** A `<ringing/>` came back: a callee device is ringing. */
        val ringing: Boolean = false,
    ) : CallState

    /** Jingle session established; [join] holds the LiveKit media credentials. */
    data class Active(
        /** The peer's FULL JID owning the remote end of the session. */
        val peer: String,
        val sid: String,
        val media: WaddleCallMedia,
        val join: WaddleLiveKitJoin,
        val initiator: String? = null,
    ) : CallState

    /** Terminal slot until the UI dismisses it back to [Idle]. */
    data class Ended(
        val sid: String,
        val reason: CallEndReason?,
    ) : CallState
}

/** The live call slot's session id, `null` only for [CallState.Idle]. */
val CallState.sidOrNull: String?
    get() = when (this) {
        CallState.Idle -> null
        is CallState.Incoming -> sid
        is CallState.Outgoing -> sid
        is CallState.Active -> sid
        is CallState.Ended -> sid
    }

/**
 * Why the call slot ended — the typed equivalent of the web reducer's
 * string reasons (`"reject" | "retract" | "expired" | "timeout" |
 * "error"` or a Jingle condition name).
 */
sealed interface CallEndReason {
    /** The peer declined via XEP-0353 `<reject/>`. */
    data object Rejected : CallEndReason

    /** The caller cancelled via XEP-0353 `<retract/>`. */
    data object Retracted : CallEndReason

    /** A XEP-0353 tie-break abort (`<tie-break/>` + `<expired/>`). */
    data object Expired : CallEndReason

    /** Local ring / session-accept timeout elapsed. */
    data object Timeout : CallEndReason

    /** A local wire send failed mid-setup. */
    data object Error : CallEndReason

    /**
     * The call finished via `<finish/>` or XEP-0166 §7.4
     * session-terminate; [reason] is the typed Jingle condition when
     * one was carried.
     */
    data class Finished(val reason: WaddleJingleReason?) : CallEndReason
}

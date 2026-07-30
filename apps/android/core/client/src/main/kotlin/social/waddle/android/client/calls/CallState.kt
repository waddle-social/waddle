package social.waddle.android.client.calls

import social.waddle.client.ffi.WaddleCallMedia
import social.waddle.client.ffi.WaddleJingleReason
import social.waddle.client.ffi.WaddleLiveKitJoin

/**
 * Whether the live slot carries a 1:1 DM call or a XEP-0272 Muji MUC
 * group call (web `CallState.kind`).
 */
enum class CallKind { DM, MUC }

/**
 * Single-slot call lifecycle state, a literal port of the web client's
 * `CallState` (chat/src/lib/calls/types.ts). One call at a time; the
 * MUC group-call flow shares the same slot (mutual exclusion with DM
 * calls by construction). Concurrent calls would need a map keyed by
 * sid.
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
    ) : CallState {
        /** Exact ready attempt that owns all local responses for this call. */
        internal var connection: CallConnection? = null
    }

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
    ) : CallState {
        /** Exact ready attempt that owns all local responses for this call. */
        internal var connection: CallConnection? = null
    }

    /**
     * XEP-0272 §Joining two-phase group-call setup in flight (web
     * `muc-pending`): preparing presence emitted, echo/peer waits and
     * the Jingle handshake with the SFU mixer still pending.
     */
    data class MucPending(
        /** Normalized bare room JID hosting the group call. */
        val roomJid: String,
        val sid: String,
        val media: WaddleCallMedia,
        /** Our XEP-0045 occupant nick in the room. */
        val selfNick: String,
        /** Our full JID, keying the preparing echo in non-anonymous rooms. */
        val selfFullJid: String?,
        /**
         * The content-declaring active presence is already room-visible,
         * so a rollback/teardown must also send the Jingle terminate.
         */
        val activePresencePublished: Boolean = false,
    ) : CallState {
        /** Exact ready attempt that owns this Muji setup. */
        internal var connection: CallConnection? = null
    }

    /** Jingle session established; [join] holds the LiveKit media credentials. */
    data class Active(
        /** The peer's FULL JID (DM) or bare room JID (MUC) owning the remote end. */
        val peer: String,
        val sid: String,
        val media: WaddleCallMedia,
        val join: WaddleLiveKitJoin,
        val initiator: String? = null,
        val kind: CallKind = CallKind.DM,
        /** Our MUC occupant nick; `null` for DM calls. */
        val selfNick: String? = null,
    ) : CallState {
        /** Exact ready attempt that owns all normal call signaling. */
        internal var connection: CallConnection? = null
    }

    /** Terminal slot until the UI dismisses it back to [Idle]. */
    data class Ended(
        val sid: String,
        val reason: CallEndReason?,
    ) : CallState
}

/**
 * Whether the slot holds a XEP-0272 MUC group-call phase. These are
 * engine-owned ([MucCallEngine]): the DM reducer must never collapse
 * them to `Ended` — the group-call teardown (leave presence, waiter
 * cancellation, mixer terminate, cache forget) runs through the
 * store's MUC end effect instead.
 */
internal val CallState.isMucCallPhase: Boolean
    get() = this is CallState.MucPending || (this is CallState.Active && kind == CallKind.MUC)

/** The live call slot's session id, `null` only for [CallState.Idle]. */
val CallState.sidOrNull: String?
    get() = when (this) {
        CallState.Idle -> null
        is CallState.Incoming -> sid
        is CallState.Outgoing -> sid
        is CallState.MucPending -> sid
        is CallState.Active -> sid
        is CallState.Ended -> sid
    }

/** The active attempt context carried by a live DM or Muji slot. */
internal val CallState.connectionOrNull: CallConnection?
    get() = when (this) {
        CallState.Idle, is CallState.Ended -> null
        is CallState.Incoming -> connection
        is CallState.Outgoing -> connection
        is CallState.MucPending -> connection
        is CallState.Active -> connection
    }

/** Incoming server events acquire the current attempt once, before reducer state changes. */
internal fun CallState.withConnectionIfMissing(connection: CallConnection?): CallState = when (this) {
    is CallState.Incoming -> copyWithConnection(connection = this.connection ?: connection)
    is CallState.Outgoing -> copyWithConnection(connection = this.connection ?: connection)
    is CallState.MucPending -> copyWithConnection(connection = this.connection ?: connection)
    is CallState.Active -> copyWithConnection(this.connection ?: connection)
    CallState.Idle, is CallState.Ended -> this
}

/** Copies retain the internal, attempt-pinned signaling capability. */
internal fun CallState.Incoming.copyWithConnection(
    accepting: Boolean = this.accepting,
    connection: CallConnection? = this.connection,
): CallState.Incoming = copy(accepting = accepting).also { it.connection = connection }

/** Copies retain the internal, attempt-pinned signaling capability. */
internal fun CallState.Outgoing.copyWithConnection(
    ringing: Boolean = this.ringing,
    connection: CallConnection? = this.connection,
): CallState.Outgoing = copy(ringing = ringing).also { it.connection = connection }

/** Copies retain the internal, attempt-pinned signaling capability. */
internal fun CallState.MucPending.copyWithConnection(
    activePresencePublished: Boolean = this.activePresencePublished,
    connection: CallConnection? = this.connection,
): CallState.MucPending = copy(activePresencePublished = activePresencePublished).also { it.connection = connection }

/** Copies retain the internal, attempt-pinned signaling capability. */
internal fun CallState.Active.copyWithConnection(
    connection: CallConnection? = this.connection,
): CallState.Active = copy().also { it.connection = connection }

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

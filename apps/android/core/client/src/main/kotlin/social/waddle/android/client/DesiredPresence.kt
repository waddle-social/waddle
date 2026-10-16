package social.waddle.android.client

import java.time.Instant

/**
 * The presence this device should advertise for its own resource
 * (RFC 6121 §4), decided by the app from what holds the stream open.
 * ADR-010 Automatic mode only: Android has no manual picker and no
 * status text until `urn:waddle:status-preference:0` reaches the FFI.
 */
sealed interface DesiredPresence {
    /**
     * Advertise nothing new. The stream exists only to finish a task (an
     * inline reply, a mark-read, a queued delivery), so the user must not
     * appear online for it. A stream that never advertised stays without
     * presence; one that already did keeps its last presence until it
     * closes, when the server broadcasts unavailable on its behalf.
     */
    data object None : DesiredPresence

    /** Plain available presence: no `<show/>`, no XEP-0319 idle. */
    data object Available : DesiredPresence

    /**
     * `<show>away</show>` (RFC 6121 §4.7.2.1), stamped with XEP-0319
     * `<idle since=…/>` when [idleSince], the last moment the user was
     * present, is known.
     */
    data class Away(val idleSince: Instant?) : DesiredPresence
}

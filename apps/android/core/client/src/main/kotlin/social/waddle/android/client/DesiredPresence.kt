package social.waddle.android.client

import java.time.Instant

/**
 * The presence this device advertises for its own resource (RFC 6121 §4).
 * The app decides the automatic one (ADR-010 Automatic mode) from what
 * holds the stream open; a manual status picked on any device of the
 * account (`urn:waddle:status-preference:0`) overrides it in
 * [social.waddle.android.client.session.SelfPresence]. Android has no
 * picker and no status text of its own.
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

    /** `<show>chat</show>`: free for chat. Only ever a manual pick. */
    data object Chat : DesiredPresence

    /** `<show>dnd</show>`: do not disturb. Only ever a manual pick. */
    data object DoNotDisturb : DesiredPresence

    /**
     * `<show>away</show>` (RFC 6121 §4.7.2.1), stamped with XEP-0319
     * `<idle since=…/>` when [idleSince], the last moment the user was
     * present, is known.
     */
    data class Away(val idleSince: Instant?) : DesiredPresence

    /**
     * `<show>xa</show>` (RFC 6121 §4.7.2.1, extended away), stamped with
     * XEP-0319 idle like [Away]. ADR-010 reaches it only automatically.
     */
    data class ExtendedAway(val idleSince: Instant?) : DesiredPresence
}

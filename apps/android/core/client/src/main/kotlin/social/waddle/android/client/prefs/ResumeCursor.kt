package social.waddle.android.client.prefs

import kotlinx.serialization.Serializable

/**
 * Per-conversation newest-seen message marker, stored as a JSON map
 * (conversation bare JID → cursor) in [SessionPrefs] — the Android
 * analog of web localStorage `waddle.chat.resume-cursors`. Consumed on
 * reconnect to decide which conversations to catch up via MAM.
 */
@Serializable
data class ResumeCursor(
    /** Newest-seen identity: stanza id, else origin id, else message id. */
    val stanzaId: String,
    /**
     * RFC 3339 timestamp of that message; live messages without a delay
     * timestamp record the local receive time. Orders cursor advances
     * and ranks DM recency for the bounded reconnect catch-up.
     */
    val timestamp: String,
)

package social.waddle.android.client

import java.time.Instant
import java.time.OffsetDateTime
import social.waddle.android.client.prefs.ResumeCursor

/**
 * In-memory per-conversation newest-seen cursors (web
 * `ReconnectCatchup` parity), advanced from the session manager's
 * fan-out on live messages and MAM results and persisted to
 * `SessionPrefs` behind a conflated write channel. Advance-only: an
 * older arrival (a MAM backfill page, an out-of-order replay) never
 * moves a cursor backwards.
 */
class ResumeCursorTracker {
    private val lock = Any()
    private val cursors = HashMap<String, ResumeCursor>()

    fun seed(persisted: Map<String, ResumeCursor>) {
        synchronized(lock) {
            cursors.clear()
            cursors.putAll(persisted)
        }
    }

    /** Returns true when the cursor moved (a persist is warranted). */
    fun advance(conversationJid: String, stanzaId: String, timestamp: String): Boolean =
        synchronized(lock) {
            val current = cursors[conversationJid]
            if (current != null && !isAtLeast(timestamp, current.timestamp)) return false
            cursors[conversationJid] = ResumeCursor(stanzaId = stanzaId, timestamp = timestamp)
            true
        }

    fun snapshot(): Map<String, ResumeCursor> = synchronized(lock) { cursors.toMap() }

    /**
     * Tracked conversations newest-first, excluding [excluding] (joined
     * rooms are enumerated separately by the catch-up) — ranks which DMs
     * the bounded reconnect catch-up refreshes.
     */
    fun newestFirst(excluding: Set<String>, limit: Int): List<String> =
        synchronized(lock) {
            cursors.entries
                .filter { it.key !in excluding }
                .sortedByDescending { parseInstant(it.value.timestamp) ?: Instant.MIN }
                .take(limit)
                .map { it.key }
        }

    fun clear() {
        synchronized(lock) { cursors.clear() }
    }

    private companion object {
        /**
         * Equal timestamps advance too (web `advance` ordering==0
         * parity): in a busy same-second burst the cursor tracks the
         * latest id. Unparseable timestamps advance — refusing would
         * pin the cursor forever.
         */
        fun isAtLeast(candidate: String, current: String): Boolean {
            val candidateInstant = parseInstant(candidate) ?: return true
            val currentInstant = parseInstant(current) ?: return true
            return candidateInstant >= currentInstant
        }

        fun parseInstant(timestamp: String): Instant? =
            runCatching { Instant.parse(timestamp) }.getOrElse {
                runCatching { OffsetDateTime.parse(timestamp).toInstant() }.getOrNull()
            }
    }
}

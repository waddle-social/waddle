package social.waddle.android.client

import java.time.OffsetDateTime

/** Now as an RFC-3339 string, the shape the recency/cursor prefs store. */
internal fun nowRfc3339(): String = OffsetDateTime.now().toString()

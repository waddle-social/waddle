package social.waddle.android.client

/** What became of a displayed-marker request ([XmppSessionManager.markConversationDisplayed]). */
enum class MarkerDispatch {
    /** Nothing left to carry: sent, already current, stale, or nothing to mark. */
    SETTLED,

    /** No live session at dispatch time: parked for the next ready pipeline's drain. */
    QUEUED,
}

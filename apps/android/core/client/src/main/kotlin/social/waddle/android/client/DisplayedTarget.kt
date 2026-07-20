package social.waddle.android.client

/**
 * A displayed dispatch target: [markerId] is what the XEP-0333 marker carries
 * (author-assigned in 1:1, room stanza id in MUCs); the stanza-id pair feeds
 * only the XEP-0490 MDS publish.
 */
data class DisplayedTarget(
    val markerId: String,
    val stanzaId: String?,
    val stanzaIdBy: String?,
    val markerRequested: Boolean,
)

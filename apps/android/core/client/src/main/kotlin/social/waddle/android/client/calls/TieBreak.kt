package social.waddle.android.client.calls

/**
 * XEP-0353 tie-breaking primitives, a byte-exact port of the web
 * client's tie-break.ts. §Tie Breaking (anchor tie-break-1): the lower
 * of the two session ids wins, where "lower" means sorted first under
 * RFC 4790 §9.3 "i;octet" collation — an unsigned byte-wise compare of
 * the UTF-8 encodings, shorter string first on a shared prefix.
 */
internal fun compareOctetStrings(left: String, right: String): Int {
    val leftBytes = left.encodeToByteArray()
    val rightBytes = right.encodeToByteArray()
    val len = minOf(leftBytes.size, rightBytes.size)
    for (i in 0 until len) {
        val diff = (leftBytes[i].toInt() and BYTE_MASK) - (rightBytes[i].toInt() and BYTE_MASK)
        if (diff != 0) return diff
    }
    return leftBytes.size - rightBytes.size
}

private fun bareCallJid(jid: String): String = jid.substringBefore('/').lowercase()

/** Case-insensitive bare-JID equality for tie-break peer matching. */
internal fun isSameCallBareJid(left: String, right: String): Boolean =
    bareCallJid(left) == bareCallJid(right)

/**
 * Whether the INCOMING propose overrules our outgoing one: true when
 * the incoming sid is lower (XEP-0353 tie-break-1), falling back to
 * the JID comparison for the unlikely equal-sid case.
 */
internal fun incomingProposeWinsTieBreak(
    incomingSid: String,
    outgoingSid: String,
    incomingFrom: String,
    ourFullJid: String,
): Boolean {
    val sidOrder = compareOctetStrings(incomingSid, outgoingSid)
    if (sidOrder != 0) return sidOrder < 0
    return compareOctetStrings(incomingFrom, ourFullJid) < 0
}

private const val BYTE_MASK = 0xFF

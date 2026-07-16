package social.waddle.android.client

import kotlinx.serialization.Serializable
import social.waddle.client.ffi.WaddlePresence

/**
 * XEP-0372 mention primitives, mirroring the web client's
 * `chat/src/lib/mentions.ts` semantics: mention URIs are `xmpp:<barejid>`
 * and the broadcast conventions are the literal `xmpp:@everyone` /
 * `xmpp:@here` URIs the Rust parser classifies as `broadcast_mention`
 * (it requires the `xmpp:` scheme plus an `@everyone`/`@here` substring).
 */

/** Broadcast mention URI addressing every member of the room. */
const val MENTION_URI_EVERYONE = "xmpp:@everyone"

/** Broadcast mention URI addressing the currently present occupants. */
const val MENTION_URI_HERE = "xmpp:@here"

/** Display token (without `@`) → broadcast URI, popover order. */
val BROADCAST_MENTIONS: Map<String, String> = mapOf(
    "everyone" to MENTION_URI_EVERYONE,
    "here" to MENTION_URI_HERE,
)

/** The XEP-0372 mention URI of [bareJid] (`xmpp:` + canonical bare JID). */
fun mentionUriFor(bareJid: String): String = "xmpp:${canonicalMentionIdentifier(bareJid)}"

/**
 * The canonical bare JID a mention URI targets, or `null` when the URI
 * addresses no user JID (broadcast mentions, malformed input). Accepts
 * bare/full JIDs too, so both sides of an equality check canonicalize
 * identically (web `mentionMatchesBareJid` parity).
 */
fun bareJidOfMentionUri(uri: String): String? {
    val canonical = canonicalMentionIdentifier(uri)
    if (!canonical.contains('@')) return null
    return canonical.substringBefore('/').ifEmpty { null }
}

/**
 * Whether a message addresses the signed-in account: any broadcast
 * mention does, otherwise one of the mention URIs must resolve to
 * [selfBareJid] (web `messageMentionsBareJid` parity).
 */
fun messageMentionsBareJid(
    broadcastMention: String?,
    mentionUris: List<String>,
    selfBareJid: String?,
): Boolean {
    if (broadcastMention != null) return true
    val self = selfBareJid?.let(::bareJidOfMentionUri) ?: return false
    return mentionUris.any { bareJidOfMentionUri(it) == self }
}

/** One row of the composer's `@` autocomplete popover. */
data class MentionCandidate(
    /** Nick as displayed (and inserted as `@display` into the body). */
    val display: String,
    /** The XEP-0372 mention URI a selection emits on the wire. */
    val uri: String,
    val isBroadcast: Boolean,
)

/**
 * Popover candidates for a room: broadcasts first, then occupants
 * (nick → latest presence) whose presence carries a real occupant JID
 * (`muc#user <item jid=…>`), alphabetically. Occupants without a real
 * JID are skipped — no conformant `xmpp:<barejid>` mention URI exists
 * for them. Nicks colliding with a broadcast identifier are filtered
 * (web `mentionAutocompleteCandidates` parity).
 */
fun mentionCandidatesOf(occupants: Map<String, WaddlePresence>): List<MentionCandidate> {
    val broadcasts = BROADCAST_MENTIONS.map { (display, uri) ->
        MentionCandidate(display = display, uri = uri, isBroadcast = true)
    }
    val seen = BROADCAST_MENTIONS.keys.toMutableSet()
    val members = occupants.entries
        .sortedBy { it.key.lowercase() }
        .mapNotNull { (nick, presence) ->
            val realJid = presence.mucJid?.let(::bareJid)?.takeIf { it.contains('@') }
            val canonical = canonicalMentionIdentifier(nick)
            if (realJid == null || canonical.isEmpty() || !seen.add(canonical)) {
                null
            } else {
                MentionCandidate(display = nick, uri = mentionUriFor(realJid), isBroadcast = false)
            }
        }
    return broadcasts + members
}

/**
 * One accepted composer mention: [begin] inclusive / [end] exclusive,
 * Unicode CODE POINTS over the trimmed send body BEFORE any reply
 * fallback prefix (XEP-0372 §3 counts code points; `preparedSend`
 * rebases onto the final wire body). Serializable so queued offline
 * sends replay with their mention references intact.
 */
@Serializable
data class MentionRef(
    val uri: String,
    val begin: UInt,
    val end: UInt,
)

/**
 * Web `canonicalMentionIdentifier` parity: trim, drop an `xmpp:` scheme,
 * drop leading `@`s, cut at the first `?`/`#`, lowercase.
 */
private fun canonicalMentionIdentifier(value: String): String {
    val trimmed = value.trim()
    val noScheme = if (trimmed.startsWith("xmpp:", ignoreCase = true)) {
        trimmed.substring("xmpp:".length)
    } else {
        trimmed
    }
    return noScheme
        .dropWhile { it == '@' }
        .substringBefore('?')
        .substringBefore('#')
        .lowercase()
}

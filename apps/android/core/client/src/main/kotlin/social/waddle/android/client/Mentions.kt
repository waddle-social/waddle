package social.waddle.android.client

import kotlinx.serialization.Serializable
import social.waddle.android.client.store.SessionStores
import social.waddle.client.ffi.WaddleMentionDirectory
import social.waddle.client.ffi.WaddleMentionTarget
import social.waddle.client.ffi.WaddlePresence
import social.waddle.client.ffi.WaddleUserSearchEntry

/**
 * Mention primitives, mirroring the web client's `chat/src/lib/mentions.ts`
 * semantics: member mentions are XEP-0372 references whose URI is
 * `xmpp:<barejid>`; room-wide mentions are XEP-0513 `#channel` mentions
 * that the Rust send builder derives from a typed `@everyone` / `@here`
 * token, and the Rust parser reports on inbound messages as
 * `broadcast_mention` (`everyone`, or `here` for an active mention).
 */

/** `broadcast_mention` of a message addressing every member of the room. */
const val BROADCAST_EVERYONE = "everyone"

/** `broadcast_mention` of a message addressing the currently present occupants. */
const val BROADCAST_HERE = "here"

/** Display token (without `@`) → broadcast kind, popover order. */
val BROADCAST_MENTIONS: Map<String, String> = mapOf(
    BROADCAST_EVERYONE to BROADCAST_EVERYONE,
    BROADCAST_HERE to BROADCAST_HERE,
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
    /**
     * The XEP-0372 mention URI a member selection emits on the wire; for a
     * broadcast, its kind ([BROADCAST_EVERYONE] / [BROADCAST_HERE]), which
     * is never sent as a reference.
     */
    val uri: String,
    val isBroadcast: Boolean,
    /** The occupant's real bare JID (avatar); `null` for broadcasts. */
    val jid: String? = null,
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
                MentionCandidate(display = nick, uri = mentionUriFor(realJid), isBroadcast = false, jid = realJid)
            }
        }
    return broadcasts + members
}

/**
 * Popover rows for XEP-0055 directory hits (people who may have no MUC
 * presence): label = directory username, URI = `xmpp:<barejid>` exactly
 * like an occupant row. Drops [selfBareJid], JIDs without a localpart,
 * blank usernames, and repeated JIDs (case-insensitive).
 */
fun directoryMentionCandidatesOf(
    entries: List<WaddleUserSearchEntry>,
    selfBareJid: String? = null,
): List<MentionCandidate> {
    val self = selfBareJid?.let(::normalizedBareJid)
    val seen = mutableSetOf<String>()
    return entries
        .filter { entry -> entry.username.isNotBlank() }
        .mapNotNull { entry ->
            val jid = normalizedBareJid(entry.jid)
            val mentionable = jid.contains('@') && jid != self
            if (mentionable && seen.add(jid)) {
                MentionCandidate(display = entry.username, uri = mentionUriFor(jid), isBroadcast = false, jid = jid)
            } else {
                null
            }
        }
}

/**
 * The room's mention directory for the Rust send builder, which turns
 * typed `@label` tokens into XEP-0372 mention references. Same occupants
 * the `@` popover offers ([mentionCandidatesOf]), then every directory
 * user seen by this room's mention searches ([directoryUsers], label =
 * username), plus the room broadcasts. Occupant labels always win: a
 * directory username colliding (case-insensitively) with an occupant
 * nick, a broadcast or an earlier directory label is skipped.
 */
fun roomMentionDirectoryOf(
    occupants: Map<String, WaddlePresence>,
    directoryUsers: List<WaddleUserSearchEntry> = emptyList(),
): WaddleMentionDirectory {
    val occupantTargets = mentionCandidatesOf(occupants).mapNotNull { candidate ->
        candidate.jid?.let { jid -> WaddleMentionTarget(label = candidate.display, jid = jid) }
    }
    val labels = (BROADCAST_MENTIONS.keys + occupantTargets.map { canonicalMentionIdentifier(it.label) })
        .toMutableSet()
    val directoryTargets = directoryMentionCandidatesOf(directoryUsers).mapNotNull { candidate ->
        val jid = candidate.jid
        if (jid == null || !labels.add(canonicalMentionIdentifier(candidate.display))) {
            null
        } else {
            WaddleMentionTarget(label = candidate.display, jid = jid)
        }
    }
    return WaddleMentionDirectory(targets = occupantTargets + directoryTargets, broadcasts = true)
}

/** [roomMentionDirectoryOf] for the current occupants of [roomJid] and its seen directory users. */
internal fun SessionStores.roomMentionDirectory(roomJid: String): WaddleMentionDirectory =
    roomMentionDirectoryOf(
        occupants = presenceStore.occupants.value[roomJid].orEmpty(),
        directoryUsers = mentionDirectoryStore.usersIn(roomJid),
    )

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

package social.waddle.android.client

import social.waddle.client.ffi.WaddleMucAffiliation
import social.waddle.client.ffi.WaddleMucRole
import social.waddle.client.ffi.WaddlePresence
import social.waddle.client.ffi.WaddlePresenceHat
import social.waddle.client.ffi.WaddleRoomBot

/**
 * Author badge derivation, ported from the web's
 * `chat/src/components/chat/message-card-badges.ts`: ONE badge renders
 * next to an author — XEP-0045 authority (owner/admin/session
 * moderator) outranks descriptive XEP-0317 hats, which are informative
 * only and never grant authority (XEP-0317 §"Hats are descriptive").
 * Ranks: OWNER=4, ADMIN=3, MOD=2, verified=1, bot/unknown hat=0;
 * authority wins ties.
 */

/** `urn:waddle:hats:bot` — the Waddle bot hat. */
const val HAT_URI_BOT = "urn:waddle:hats:bot"

/** `urn:waddle:hats:verified` — the Waddle verified hat. */
const val HAT_URI_VERIFIED = "urn:waddle:hats:verified"

/** What a badge represents, for theming. */
enum class AuthorBadgeKind { OWNER, ADMIN, MODERATOR, VERIFIED, BOT, HAT }

/** The single badge rendered next to an author name. */
data class AuthorBadge(val kind: AuthorBadgeKind, val label: String, val rank: Int)

private const val RANK_OWNER = 4
private const val RANK_ADMIN = 3
private const val RANK_MODERATOR = 2
private const val RANK_VERIFIED = 1
private const val RANK_HAT = 0

private val BOT_BADGE = AuthorBadge(AuthorBadgeKind.BOT, "BOT", RANK_HAT)

/** Web `authorityBadge`: XEP-0045 affiliation/role → badge. */
fun authorityBadge(affiliation: WaddleMucAffiliation?, role: WaddleMucRole?): AuthorBadge? = when {
    affiliation == WaddleMucAffiliation.OWNER ->
        AuthorBadge(AuthorBadgeKind.OWNER, "OWNER", RANK_OWNER)
    affiliation == WaddleMucAffiliation.ADMIN ->
        AuthorBadge(AuthorBadgeKind.ADMIN, "ADMIN", RANK_ADMIN)
    // Role=moderator without owner/admin affiliation is the
    // "promoted for this session" case — still authoritative.
    role == WaddleMucRole.MODERATOR ->
        AuthorBadge(AuthorBadgeKind.MODERATOR, "MOD", RANK_MODERATOR)
    else -> null
}

/** The server-assigned `urn:waddle:hats:bot` hat is among [hats]. */
fun hasBotHat(hats: List<WaddlePresenceHat>): Boolean = hats.any { it.uri == HAT_URI_BOT }

/**
 * Bare JIDs the server declares as bots: the room's bot list (XEP-0030
 * disco#items `urn:waddle:room:bots:0`, bots hold no affiliation) plus
 * the JIDs it hatted `urn:waddle:hats:bot` this session. Normalized
 * for [messageAuthorBadgeOf].
 */
fun declaredBotJidsOf(roomBots: List<WaddleRoomBot>, hatLearned: Set<String>): Set<String> =
    (roomBots.map { it.jid } + hatLearned).mapTo(HashSet(), ::normalizedBareJid)

/**
 * Web `descriptiveBadge`: highest-ranked hat, first-wins on ties
 * (strict `>` comparison). Unknown hats show their server title.
 */
fun descriptiveBadge(hats: List<WaddlePresenceHat>): AuthorBadge? {
    var best: AuthorBadge? = null
    for (hat in hats) {
        val candidate = when (hat.uri) {
            HAT_URI_BOT -> BOT_BADGE
            HAT_URI_VERIFIED -> AuthorBadge(AuthorBadgeKind.VERIFIED, "VERIFIED", RANK_VERIFIED)
            else -> AuthorBadge(AuthorBadgeKind.HAT, hat.title, RANK_HAT)
        }
        if (best == null || candidate.rank > best.rank) best = candidate
    }
    return best
}

/**
 * Web `authorBadge`: the single winner — authority beats hats on ties
 * (`>=` comparison).
 */
fun authorBadgeOf(presence: WaddlePresence?): AuthorBadge? {
    val fromAuthority = authorityBadge(presence?.mucAffiliation, presence?.mucRole)
    val fromHats = descriptiveBadge(presence?.hats.orEmpty())
    return when {
        fromAuthority != null && fromHats != null ->
            if (fromAuthority.rank >= fromHats.rank) fromAuthority else fromHats
        else -> fromAuthority ?: fromHats
    }
}

/**
 * The badge beside a message author. A pinned [authorJid] the server
 * declares a bot ([declaredBotJids]) is a BOT outright — [presenceOf]
 * (the nick lookup, which a later occupant may have reused) is only
 * consulted for everyone else, whose hats/authority apply as usual.
 */
fun messageAuthorBadgeOf(
    authorJid: String?,
    declaredBotJids: Set<String>,
    presenceOf: () -> WaddlePresence?,
): AuthorBadge? =
    if (authorJid != null && normalizedBareJid(authorJid) in declaredBotJids) {
        BOT_BADGE
    } else {
        authorBadgeOf(presenceOf())
    }

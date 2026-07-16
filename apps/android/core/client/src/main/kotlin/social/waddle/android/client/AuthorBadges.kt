package social.waddle.android.client

import social.waddle.client.ffi.WaddleMucAffiliation
import social.waddle.client.ffi.WaddleMucRole
import social.waddle.client.ffi.WaddlePresence
import social.waddle.client.ffi.WaddlePresenceHat

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

/**
 * Web `descriptiveBadge`: highest-ranked hat, first-wins on ties
 * (strict `>` comparison). Unknown hats show their server title.
 */
fun descriptiveBadge(hats: List<WaddlePresenceHat>): AuthorBadge? {
    var best: AuthorBadge? = null
    for (hat in hats) {
        val candidate = when (hat.uri) {
            HAT_URI_BOT -> AuthorBadge(AuthorBadgeKind.BOT, "BOT", RANK_HAT)
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

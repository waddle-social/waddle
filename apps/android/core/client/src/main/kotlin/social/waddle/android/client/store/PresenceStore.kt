package social.waddle.android.client.store

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import social.waddle.android.client.hasBotHat
import social.waddle.android.client.normalizedBareJid
import social.waddle.android.client.resourcepart
import social.waddle.client.ffi.WaddlePresence

/**
 * The Show to render for a JID, in ADR-010 most-available order:
 * declaration order IS the rank (a deliberate Do Not Disturb wins over
 * every other online state; offline is the floor). RFC 6121 `chat`
 * renders as [AVAILABLE].
 */
enum class PresenceShow {
    DND,
    AVAILABLE,
    AWAY,
    XA,
    OFFLINE,
    ;

    /** Whether this Show ranks above [other] (most-available-wins). */
    fun outranks(other: PresenceShow): Boolean = ordinal < other.ordinal
}

/** The rendered Show of one presence (`unavailable` and errors are offline). */
fun presenceShowOf(presence: WaddlePresence): PresenceShow =
    if (presence.presenceType != PRESENCE_AVAILABLE) {
        PresenceShow.OFFLINE
    } else {
        when (presence.show) {
            "dnd" -> PresenceShow.DND
            "away" -> PresenceShow.AWAY
            "xa" -> PresenceShow.XA
            else -> PresenceShow.AVAILABLE
        }
    }

/**
 * One contact's presence collapsed across its resources
 * (most-available-wins, ADR-010). [presence] is the most recently
 * received presence among the resources carrying [show]: the one whose
 * status and XEP-0319 idle stamp the UI renders.
 */
data class ContactPresence(
    /** [normalizedBareJid] of the contact. */
    val bareJid: String,
    val show: PresenceShow,
    val presence: WaddlePresence,
)

/**
 * Presence bookkeeping for one stream: MUC occupant presence per room
 * (nick → latest presence, `unavailable` removes) and contact presence
 * per bare JID → resource, collapsed into [contacts].
 *
 * Every key is the [normalizedBareJid] form (room keys match
 * [OccupantJidStore] and the Muji bookkeeping), and both public maps
 * are [BareJidMap]s, so a lookup by any case variant resolves.
 *
 * Only availability presence is stored: subscription management
 * (`subscribe`/`subscribed`/`unsubscribe`/`unsubscribed`), `probe` and
 * `error` presences say nothing about whether someone is here. The
 * account's own resources are never a contact.
 *
 * Android never resumes a stream, so every ready is fresh and the
 * server re-sends current presence; [resetForFreshSession] drops what
 * the last stream knew, or someone who left while we were disconnected
 * would linger forever.
 */
class PresenceStore {
    private val lock = Any()

    private val _occupants = MutableStateFlow<Map<String, Map<String, WaddlePresence>>>(BareJidMap.empty())

    /** room bare JID (normalised) → (nick → latest presence). */
    val occupants: StateFlow<Map<String, Map<String, WaddlePresence>>> = _occupants.asStateFlow()

    /** Guarded by [lock]: bare JID → (resource, `""` when bare → presence), newest last. */
    private val contactResources = HashMap<String, LinkedHashMap<String, WaddlePresence>>()

    private val _contacts = MutableStateFlow<Map<String, ContactPresence>>(BareJidMap.empty())

    /**
     * Online contacts: bare JID (normalised) → presence collapsed across
     * resources. Absent means offline (or never seen this stream).
     */
    val contacts: StateFlow<Map<String, ContactPresence>> = _contacts.asStateFlow()

    private val _botJids = MutableStateFlow<Set<String>>(emptySet())

    /**
     * Real bare JIDs the server hatted `urn:waddle:hats:bot`, kept for
     * the session (web parity): a bot's room presence is lazy and comes
     * and goes, its identity does not.
     */
    val botJids: StateFlow<Set<String>> = _botJids.asStateFlow()

    private val _ownBareJid = MutableStateFlow<String?>(null)

    /** The signed-in account (normalised); never a contact. */
    val ownBareJid: StateFlow<String?> = _ownBareJid.asStateFlow()

    /** The signed-in account; its own resources are never a contact. */
    fun setOwnBareJid(jid: String?) {
        _ownBareJid.value = jid?.let(::normalizedBareJid)
    }

    fun onPresence(presence: WaddlePresence) {
        val from = presence.from ?: return
        val type = presence.presenceType
        if (type != PRESENCE_AVAILABLE && type != PRESENCE_UNAVAILABLE) return
        val nick = resourcepart(from)
        synchronized(lock) {
            if (isMucOccupant(presence) && nick != null) {
                presence.mucJid?.takeIf { hasBotHat(presence.hats) }
                    ?.let { real -> _botJids.value += normalizedBareJid(real) }
                updateOccupant(room = normalizedBareJid(from), nick = nick, presence = presence)
            } else {
                updateContact(bare = normalizedBareJid(from), resource = nick.orEmpty(), presence = presence)
            }
        }
    }

    /**
     * A fresh stream is ready: forget every contact and occupant the
     * previous stream reported (the server re-sends current presence on
     * the new one). Bot identities are kept for the session.
     */
    fun resetForFreshSession() {
        synchronized(lock) {
            contactResources.clear()
            _contacts.value = BareJidMap.empty()
            _occupants.value = BareJidMap.empty()
        }
    }

    fun clear() {
        synchronized(lock) {
            contactResources.clear()
            _contacts.value = BareJidMap.empty()
            _occupants.value = BareJidMap.empty()
            _botJids.value = emptySet()
            _ownBareJid.value = null
        }
    }

    private fun updateOccupant(room: String, nick: String, presence: WaddlePresence) {
        val rooms = _occupants.value
        val occupants = rooms[room].orEmpty()
        val next =
            if (presence.presenceType == PRESENCE_UNAVAILABLE) {
                occupants - nick
            } else {
                occupants + (nick to presence)
            }
        _occupants.value = BareJidMap(if (next.isEmpty()) rooms - room else rooms + (room to next))
    }

    private fun updateContact(bare: String, resource: String, presence: WaddlePresence) {
        if (bare == _ownBareJid.value) return
        val resources = contactResources.getOrPut(bare) { LinkedHashMap() }
        when {
            presence.presenceType == PRESENCE_AVAILABLE -> {
                // Re-insert so the newest presence iterates last.
                resources.remove(resource)
                resources[resource] = presence
            }
            // RFC 6121 §4.5.3: unavailable from the bare JID covers
            // every resource of the account.
            resource.isEmpty() -> resources.clear()
            else -> resources.remove(resource)
        }
        val collapsed = collapse(bare, resources)
        if (collapsed == null) contactResources.remove(bare)
        val contacts = _contacts.value
        val next = if (collapsed == null) contacts - bare else contacts + (bare to collapsed)
        if (next != contacts) _contacts.value = BareJidMap(next)
    }

    private fun collapse(bare: String, resources: Map<String, WaddlePresence>): ContactPresence? {
        var best: ContactPresence? = null
        for (presence in resources.values) {
            val show = presenceShowOf(presence)
            // `!outranks` keeps the newer of equal-ranked resources.
            if (best == null || !best.show.outranks(show)) {
                best = ContactPresence(bareJid = bare, show = show, presence = presence)
            }
        }
        return best
    }

    private fun isMucOccupant(presence: WaddlePresence): Boolean =
        presence.mucRole != null ||
            presence.mucAffiliation != null ||
            presence.mucJid != null ||
            presence.mucStatusCodes.isNotEmpty()
}

private const val PRESENCE_AVAILABLE = "available"
private const val PRESENCE_UNAVAILABLE = "unavailable"

/**
 * A read-only map keyed by [normalizedBareJid] whose lookups normalise
 * the key too, so `map[jid]` resolves whatever case the caller holds.
 * Equality and iteration are those of the normalised entries.
 */
class BareJidMap<V> internal constructor(private val normalized: Map<String, V>) : Map<String, V> by normalized {
    override fun get(key: String): V? = normalized[normalizedBareJid(key)]

    override fun containsKey(key: String): Boolean = normalized.containsKey(normalizedBareJid(key))

    override fun equals(other: Any?): Boolean = normalized == other

    override fun hashCode(): Int = normalized.hashCode()

    override fun toString(): String = normalized.toString()

    internal companion object {
        fun <V> empty(): BareJidMap<V> = BareJidMap(emptyMap())
    }
}

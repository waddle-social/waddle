package social.waddle.android.feature.conversation

import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.getValue
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import social.waddle.android.LocalAppGraph
import social.waddle.android.client.normalizedBareJid
import social.waddle.android.client.store.ContactPresence
import social.waddle.android.client.store.DirectoryPerson
import social.waddle.android.client.store.presenceShowOf
import social.waddle.android.feature.members.PeerProfile
import social.waddle.android.feature.members.PeerProfileSheet
import social.waddle.client.ffi.WaddlePresence

/** Who a message row's avatar and name stand for, as far as the row knows. */
@Immutable
data class MessageAuthor(
    val name: String,
    /** Real bare JID; `null` for an anonymous room occupant. */
    val jid: String?,
    /** Room nick, for the occupant's presence (room rows only). */
    val nick: String?,
)

/**
 * The profile a message author opens: [occupant] is their presence in
 * this room by nick, used only while that nick still belongs to them (a
 * nick can pass to someone else after the message); [person] and
 * [contact] are what the rest of the stream knows of their bare JID.
 * Room role and hats come from the occupant presence; nothing is shown
 * that is not known.
 */
fun authorProfileOf(
    author: MessageAuthor,
    occupant: WaddlePresence?,
    person: DirectoryPerson?,
    contact: ContactPresence?,
): PeerProfile {
    val sameOccupant = occupant?.takeIf { presence ->
        val occupantJid = presence.mucJid?.let(::normalizedBareJid)
        author.jid == null || occupantJid == null || occupantJid == normalizedBareJid(author.jid)
    }
    val status = (sameOccupant?.status ?: contact?.presence?.status)?.trim()?.takeIf { it.isNotEmpty() }
    return PeerProfile(
        displayName = author.name,
        jid = author.jid,
        presence = person?.show ?: sameOccupant?.let(::presenceShowOf) ?: contact?.show,
        statusText = status,
        affiliation = sameOccupant?.mucAffiliation,
        hats = sameOccupant?.hats?.map { it.title }.orEmpty(),
    )
}

/**
 * Whether "Message" can open a DM with [jid]: a real bare JID that is
 * neither the reader nor the peer of the DM already open ([dmPeerJid]).
 */
fun canMessage(jid: String?, selfBareJid: String?, dmPeerJid: String?): Boolean {
    val bare = jid?.let(::normalizedBareJid)?.takeIf { it.isNotEmpty() } ?: return false
    return bare != selfBareJid?.let(::normalizedBareJid) && bare != dmPeerJid?.let(::normalizedBareJid)
}

/**
 * The profile sheet a message avatar or author name opens, filled from
 * the session's presence. No moderation verbs: the conversation offers
 * none for a person (members does).
 */
@Composable
internal fun MessageAuthorProfileSheet(
    author: MessageAuthor,
    occupant: WaddlePresence?,
    onDismiss: () -> Unit,
    onMessage: (() -> Unit)?,
) {
    val sessionManager = LocalAppGraph.current.sessionManager
    val directory by sessionManager.peopleDirectory.collectAsStateWithLifecycle()
    val contacts by sessionManager.presenceStore.contacts.collectAsStateWithLifecycle()
    val bare = author.jid?.let(::normalizedBareJid)
    val profile = authorProfileOf(
        author = author,
        occupant = occupant,
        person = bare?.let { jid -> directory.people.firstOrNull { it.bareJid == jid } },
        contact = bare?.let { contacts[it] },
    )
    PeerProfileSheet(
        profile = profile,
        presenceLine = profile.presence?.let { presenceWordOf(it, idleAge = null) },
        onDismiss = onDismiss,
        onMessage = onMessage,
    )
}

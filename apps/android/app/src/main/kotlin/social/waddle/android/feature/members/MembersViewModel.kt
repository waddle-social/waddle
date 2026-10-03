package social.waddle.android.feature.members

import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import social.waddle.android.AppGraph
import social.waddle.android.client.RoomAdminResult
import social.waddle.android.client.XmppEvent
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.client.canManageMembersOf
import social.waddle.android.client.hasBotHat
import social.waddle.android.client.normalizedBareJid
import social.waddle.android.client.store.MemberListStatus
import social.waddle.android.client.store.RoomMembersState
import social.waddle.android.jid.bareJidOf
import social.waddle.android.jid.localpartOf
import social.waddle.android.viewModelFactoryOf
import social.waddle.client.ffi.WaddleMucAffiliation
import social.waddle.client.ffi.WaddlePresence
import social.waddle.client.ffi.WaddleRoomBot
import social.waddle.client.ffi.WaddleUserSearchEntry

/** One row of the members screen: an authoritative §9.5 list entry
 *  merged with live occupancy, or a presence-only inferred occupant. */
data class MemberRow(
    /** Bare JID; `null` for presence-only occupants of a (semi-)anonymous room. */
    val jid: String?,
    val displayName: String,
    val affiliation: WaddleMucAffiliation,
    /** Occupant nick while present (the §8.2 kick address). */
    val nick: String?,
    val presentNow: Boolean,
    /** XEP-0317 hat titles from the occupant presence. */
    val hats: List<String>,
    /** Presence-only row: shown, never editable (web parity). */
    val inferred: Boolean,
)

/**
 * A bot the server lists for the room (XEP-0030 `urn:waddle:room:bots:0`).
 * Bots hold no affiliation and their room presence is momentary, so a
 * bot row carries no presence and offers no actions.
 */
data class BotRow(val jid: String, val displayName: String)

data class MembersUiState(
    val status: MemberListStatus = MemberListStatus.LOADING,
    val rows: List<MemberRow> = emptyList(),
    /** Declared room bots, by name (own section, after the people). */
    val bots: List<BotRow> = emptyList(),
    /** Own affiliation ∈ {owner, admin} (web `canManageMembers`). */
    val canManage: Boolean = false,
    val searchQuery: String = "",
    val searchResults: List<WaddleUserSearchEntry> = emptyList(),
)

/**
 * Room member management (web `MemberManagement.vue` parity): the four
 * per-affiliation `muc#admin` lists merged with live occupants from
 * presence, plus promote/demote/remove/ban/kick actions and the
 * XEP-0055 add-member search. Every mutation is server-authorized;
 * [MembersUiState.canManage] only hides UI the server would refuse.
 */
class MembersViewModel(
    private val sessionManager: XmppSessionManager,
    private val roomJid: String,
) : ViewModel() {
    private val search = MutableStateFlow(SearchState())
    private var searchJob: Job? = null
    private var searchTicket = 0

    /** Real bot JIDs already asked about this load: one refetch per missing JID. */
    private val requestedBots = HashSet<String>()

    private val _actionFailures = MutableSharedFlow<RoomAdminResult>(extraBufferCapacity = 4)

    /** Refused/failed member actions, for the screen's snackbar. */
    val actionFailures: SharedFlow<RoomAdminResult> = _actionFailures

    val uiState: StateFlow<MembersUiState> = combine(
        sessionManager.roomMembersStore.rooms,
        sessionManager.presenceStore.occupants,
        search,
    ) { rooms, occupants, searchState ->
        val members = rooms[roomJid] ?: RoomMembersState()
        val roomOccupants = occupants[roomJid].orEmpty()
        MembersUiState(
            status = members.status,
            rows = memberRowsOf(members, roomOccupants),
            bots = botRowsOf(members.bots),
            canManage = canManageMembersOf(roomOccupants),
            searchQuery = searchState.query,
            searchResults = searchState.results,
        )
    }.stateIn(viewModelScope, SharingStarted.Eagerly, MembersUiState())

    init {
        refresh()
        viewModelScope.launch {
            sessionManager.events.collect { event ->
                if (event is XmppEvent.Presence) refreshBotsIfUndeclared(event.presence)
            }
        }
    }

    /**
     * A bot joins a room for one send and leaves, and the server records
     * it in the room's bot list at join. So a bot-hatted presence here —
     * available or unavailable — whose real JID the list does not name
     * means the list is stale: ask again, once per such JID per load.
     */
    private fun refreshBotsIfUndeclared(presence: WaddlePresence) {
        if (presence.from?.let(::bareJidOf) != roomJid || !hasBotHat(presence.hats)) return
        val bot = presence.mucJid?.let(::normalizedBareJid) ?: return
        val listed = sessionManager.roomMembersStore.rooms.value[roomJid]
            ?.bots.orEmpty().mapTo(HashSet()) { normalizedBareJid(it.jid) }
        if (bot in listed || !requestedBots.add(bot)) return
        viewModelScope.launch { sessionManager.refreshRoomBots(roomJid) }
    }

    /** Re-run the four-tier §9.5 fan-out into the members store. */
    fun refresh() {
        requestedBots.clear()
        viewModelScope.launch { sessionManager.refreshRoomMembers(roomJid) }
    }

    /** Promote/demote to [affiliation]; `NONE` removes membership (§5.2). */
    fun setAffiliation(row: MemberRow, affiliation: WaddleMucAffiliation) {
        val target = row.jid ?: return
        runAction { sessionManager.setRoomAffiliation(roomJid, target, affiliation) }
    }

    /** §9.1 ban: affiliation → outcast. */
    fun ban(row: MemberRow) = setAffiliation(row, WaddleMucAffiliation.OUTCAST)

    /** Remove from the member list: affiliation → none (web parity). */
    fun remove(row: MemberRow) = setAffiliation(row, WaddleMucAffiliation.NONE)

    /** §8.2 kick by nick: ejects the occupant, affiliation kept. */
    fun kick(row: MemberRow) {
        val nick = row.nick ?: return
        runAction(refreshAfter = false) { sessionManager.kickOccupant(roomJid, nick) }
    }

    /** Add a searched user as member, then clear the search. */
    fun addMember(entry: WaddleUserSearchEntry) {
        runAction { sessionManager.setRoomAffiliation(roomJid, entry.jid, WaddleMucAffiliation.MEMBER) }
        clearSearch()
    }

    /** Debounced XEP-0055 search (220 ms, web parity), managers only. */
    fun onSearchQueryChanged(query: String) {
        search.update { it.copy(query = query) }
        searchJob?.cancel()
        val ticket = ++searchTicket
        val trimmed = query.trim()
        if (trimmed.isEmpty() || !uiState.value.canManage) {
            search.update { it.copy(results = emptyList()) }
            return
        }
        searchJob = viewModelScope.launch {
            delay(SEARCH_DEBOUNCE_MS)
            val memberJids = uiState.value.rows.mapNotNull { it.jid }.toSet()
            val results = sessionManager.searchUsers(trimmed).orEmpty()
                .filter { it.jid !in memberJids }
            if (ticket == searchTicket) {
                search.update { it.copy(results = results) }
            }
        }
    }

    fun clearSearch() {
        searchJob?.cancel()
        searchTicket += 1
        search.value = SearchState()
    }

    private fun runAction(
        refreshAfter: Boolean = true,
        action: suspend () -> RoomAdminResult,
    ) {
        viewModelScope.launch {
            when (val result = action()) {
                RoomAdminResult.Ok -> if (refreshAfter) sessionManager.refreshRoomMembers(roomJid)
                else -> _actionFailures.tryEmit(result)
            }
        }
    }

    private data class SearchState(
        val query: String = "",
        val results: List<WaddleUserSearchEntry> = emptyList(),
    )

    companion object {
        private const val SEARCH_DEBOUNCE_MS = 220L

        fun factory(graph: AppGraph, roomJid: String): ViewModelProvider.Factory =
            viewModelFactoryOf {
                MembersViewModel(
                    sessionManager = graph.sessionManager,
                    roomJid = roomJid,
                )
            }
    }
}

/** Web member sort parity: owner → admin → member → outcast, then name. */
private fun tierOrderOf(affiliation: WaddleMucAffiliation): Int = when (affiliation) {
    WaddleMucAffiliation.OWNER -> 0
    WaddleMucAffiliation.ADMIN -> 1
    WaddleMucAffiliation.MEMBER -> 2
    WaddleMucAffiliation.OUTCAST -> 3
    WaddleMucAffiliation.NONE -> 4
}

/**
 * Merge the authoritative member list with live occupants: list rows
 * gain "present now" + nick + hats when an occupant's real JID
 * matches; occupants missing from the list append as read-only
 * inferred rows (web `mergeMentionMembers` behavior). A momentary
 * occupant the server hatted `urn:waddle:hats:bot` is a bot (see
 * [botRowsOf]), never a person.
 */
internal fun memberRowsOf(
    members: RoomMembersState,
    occupants: Map<String, WaddlePresence>,
): List<MemberRow> {
    val occupantsByBareJid = occupants.entries
        .mapNotNull { (nick, presence) ->
            presence.mucJid?.let { real -> bareJidOf(real) to (nick to presence) }
        }
        .toMap()
    val listedJids = members.members.map { it.jid }.toSet()
    val listed = members.members.map { entry ->
        val occupant = occupantsByBareJid[entry.jid]
        MemberRow(
            jid = entry.jid,
            displayName = entry.nick ?: localpartOf(entry.jid),
            affiliation = entry.affiliation,
            nick = occupant?.first,
            presentNow = occupant != null,
            hats = occupant?.second?.hats?.map { it.title }.orEmpty(),
            inferred = false,
        )
    }
    val inferred = occupants.entries
        .filter { (_, presence) ->
            val bare = presence.mucJid?.let(::bareJidOf)
            !hasBotHat(presence.hats) && (bare == null || bare !in listedJids)
        }
        .map { (nick, presence) ->
            MemberRow(
                jid = presence.mucJid?.let(::bareJidOf),
                displayName = nick,
                affiliation = presence.mucAffiliation ?: WaddleMucAffiliation.NONE,
                nick = nick,
                presentNow = true,
                hats = presence.hats.map { it.title },
                inferred = true,
            )
        }
    return (listed + inferred).sortedWith(
        compareBy({ tierOrderOf(it.affiliation) }, { it.displayName.lowercase() }),
    )
}

/** The room's declared bots by name (the service's name, else the JID localpart). */
internal fun botRowsOf(bots: List<WaddleRoomBot>): List<BotRow> =
    bots.map { BotRow(jid = it.jid, displayName = it.name?.takeIf(String::isNotBlank) ?: localpartOf(it.jid)) }
        .sortedBy { it.displayName.lowercase() }

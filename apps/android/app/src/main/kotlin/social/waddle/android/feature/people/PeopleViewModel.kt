package social.waddle.android.feature.people

import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.stateIn
import social.waddle.android.AppGraph
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.client.normalizedBareJid
import social.waddle.android.client.store.DirectoryPerson
import social.waddle.android.feature.rooms.topologySettledFlow
import social.waddle.android.viewModelFactoryOf

data class PeopleUiState(
    /** The directory: everyone in joined rooms with a real JID, plus online contacts; no self, no bots. */
    val people: List<DirectoryPerson> = emptyList(),
    /** Normalised room JID → display name, for "in the huddle in storage". */
    val roomNames: Map<String, String> = emptyMap(),
    /** False until the topology (and with it the room joins) had its chance to load. */
    val settled: Boolean = false,
)

/** The People tab: who is around, from the shared directory projection. */
class PeopleViewModel(sessionManager: XmppSessionManager) : ViewModel() {
    val uiState: StateFlow<PeopleUiState> = combine(
        sessionManager.peopleDirectory,
        sessionManager.roomStore.channels,
        topologySettledFlow(sessionManager),
    ) { directory, channels, settled ->
        PeopleUiState(
            people = directory.people,
            roomNames = channels.associate { normalizedBareJid(it.roomJid) to (it.bookmarkName ?: it.name) },
            settled = settled,
        )
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), PeopleUiState())

    companion object {
        fun factory(graph: AppGraph): ViewModelProvider.Factory =
            viewModelFactoryOf { PeopleViewModel(graph.sessionManager) }
    }
}

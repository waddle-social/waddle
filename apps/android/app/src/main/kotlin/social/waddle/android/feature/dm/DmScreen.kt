package social.waddle.android.feature.dm

import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.res.stringResource
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import kotlinx.coroutines.delay
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.client.IdleAge
import social.waddle.android.client.idleAgeOf
import social.waddle.android.client.presenceShowsIdle
import social.waddle.android.feature.conversation.ConversationScreen
import social.waddle.android.feature.conversation.trustedPreviewOriginOf
import social.waddle.android.feature.search.MessageSearchTarget
import social.waddle.android.jid.bareJidOf
import social.waddle.client.ffi.WaddlePresence

/** DM timeline + composer over the shared conversation scaffold. */
@Composable
fun DmScreen(
    peerJid: String,
    name: String,
    onBack: () -> Unit,
    onOpenThread: (threadId: String) -> Unit,
) {
    val graph = LocalAppGraph.current
    val viewModel: DmViewModel = viewModel(
        key = "dm:$peerJid",
        factory = DmViewModel.factory(graph, peerJid),
    )
    val session by graph.currentSession.collectAsStateWithLifecycle()
    val contacts by graph.sessionManager.presenceStore.contacts.collectAsStateWithLifecycle()
    ConversationScreen(
        title = name,
        viewModel = viewModel,
        onBack = onBack,
        onOpenThread = onOpenThread,
        searchTarget = MessageSearchTarget(peerJid, isGroupchat = false),
        selfBareJid = session?.jid?.let(::bareJidOf),
        subtitle = dmPresenceSubtitle(contacts[peerJid]),
        trustedMediaOrigin = trustedPreviewOriginOf(session, graph.serverUrl),
    )
}

/**
 * Web `ChatHeader.dmPeerPresenceText` parity: "away · idle 20m" while
 * the peer's presence show is `away`/`xa` and XEP-0319 `idleSince` is
 * present; bare "away" without it; nothing otherwise. The idle age
 * re-derives on a minute ticker so it stays current without presence
 * traffic.
 */
@Composable
private fun dmPresenceSubtitle(presence: WaddlePresence?): String? {
    if (presence == null || !presenceShowsIdle(presence.show)) return null
    val idleSince = presence.idleSince
        ?: return stringResource(R.string.dm_presence_away)
    var nowMs by remember { mutableLongStateOf(System.currentTimeMillis()) }
    LaunchedEffect(idleSince) {
        while (true) {
            delay(MINUTE_MS)
            nowMs = System.currentTimeMillis()
        }
    }
    val age = idleAgeOf(idleSince, nowMs)
        ?: return stringResource(R.string.dm_presence_away)
    return stringResource(R.string.dm_presence_away_idle, idleAgeText(age))
}

@Composable
private fun idleAgeText(age: IdleAge): String = when (age) {
    IdleAge.UnderMinute -> stringResource(R.string.idle_under_minute)
    is IdleAge.Minutes -> stringResource(R.string.idle_minutes, age.minutes)
    is IdleAge.Hours -> stringResource(R.string.idle_hours, age.hours)
    is IdleAge.Days -> stringResource(R.string.idle_days, age.days)
}

private const val MINUTE_MS = 60_000L

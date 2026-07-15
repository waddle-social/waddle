package social.waddle.android

import android.Manifest
import android.content.pm.PackageManager
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.flow.StateFlow
import social.waddle.android.client.WaddleAppState
import social.waddle.android.feature.login.LoginScreen
import social.waddle.android.navigation.WaddleNavDisplay
import social.waddle.android.navigation.WaddleNavKey
import social.waddle.android.navigation.conversationNavKeyFor

/**
 * App-state switch (web `AppShell.vue` parity): Loading → progress,
 * SignedOut → login, Error → retry, Ready → the navigation shell.
 */
@Composable
fun AppShell(
    pendingConversationJid: StateFlow<String?>,
    onConversationConsumed: () -> Unit,
) {
    val graph = LocalAppGraph.current
    val appState by graph.appState.collectAsStateWithLifecycle()
    Surface(modifier = Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
        when (val state = appState) {
            WaddleAppState.Loading -> LoadingScreen()
            WaddleAppState.SignedOut -> LoginScreen()
            is WaddleAppState.Error -> RestoreErrorScreen(
                detail = state.message,
                onRetry = graph.bootstrap::restore,
            )
            WaddleAppState.Ready -> ReadyShell(
                pendingConversationJid = pendingConversationJid,
                onConversationConsumed = onConversationConsumed,
            )
        }
    }
}

@Composable
private fun LoadingScreen() {
    Box(modifier = Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
        CircularProgressIndicator()
    }
}

@Composable
private fun RestoreErrorScreen(detail: String, onRetry: () -> Unit) {
    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(32.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp, Alignment.CenterVertically),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(
            text = stringResource(R.string.restore_error_title),
            style = MaterialTheme.typography.headlineSmall,
            textAlign = TextAlign.Center,
        )
        Text(
            text = detail,
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            textAlign = TextAlign.Center,
        )
        Button(onClick = onRetry) {
            Text(text = stringResource(R.string.action_retry))
        }
    }
}

@Composable
private fun ReadyShell(
    pendingConversationJid: StateFlow<String?>,
    onConversationConsumed: () -> Unit,
) {
    val graph = LocalAppGraph.current
    RequestNotificationPermissionOnce()

    WaddleNavDisplay { backStack ->
        val pendingJid by pendingConversationJid.collectAsStateWithLifecycle()
        LaunchedEffect(pendingJid) {
            val jid = pendingJid ?: return@LaunchedEffect
            val key = conversationNavKeyFor(
                topology = graph.sessionManager.roomStore.topology.value,
                joinedRooms = graph.sessionManager.roomStore.joinedRooms.value,
                conversationJid = jid,
            )
            if (backStack.lastOrNull() != key) {
                backStack.add(key)
            }
            if (key is WaddleNavKey.Channel) {
                graph.sessionManager.joinRoom(
                    roomJid = key.roomJid,
                    nick = graph.currentSession.value?.xmppLocalpart ?: DEFAULT_NICK,
                )
            }
            onConversationConsumed()
        }
    }
}

/** POST_NOTIFICATIONS runtime prompt, fired once on the first Ready shell. */
@Composable
private fun RequestNotificationPermissionOnce() {
    val context = LocalContext.current
    val launcher = rememberLauncherForActivityResult(
        contract = ActivityResultContracts.RequestPermission(),
        onResult = {},
    )
    LaunchedEffect(Unit) {
        val granted = ContextCompat.checkSelfPermission(
            context,
            Manifest.permission.POST_NOTIFICATIONS,
        ) == PackageManager.PERMISSION_GRANTED
        if (!granted) {
            launcher.launch(Manifest.permission.POST_NOTIFICATIONS)
        }
    }
}

/** Fallback MUC nick when the session is briefly unavailable. */
internal const val DEFAULT_NICK = "waddle"

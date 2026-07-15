package social.waddle.android

import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.core.splashscreen.SplashScreen.Companion.installSplashScreen
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.flow.MutableStateFlow
import social.waddle.android.client.WaddleAppState
import social.waddle.android.client.prefs.ThemeMode
import social.waddle.android.theme.WaddleTheme

/**
 * The single activity: edge-to-edge, splash held until the app state
 * leaves `Loading`, and `waddle.navigate.jid` intent extras (notification
 * taps) funneled into the app shell for navigation.
 */
class MainActivity : ComponentActivity() {
    private val pendingConversationJid = MutableStateFlow<String?>(null)

    override fun onCreate(savedInstanceState: Bundle?) {
        val splashScreen = installSplashScreen()
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        val graph = (application as WaddleApplication).graph
        splashScreen.setKeepOnScreenCondition {
            graph.appState.value is WaddleAppState.Loading
        }
        pendingConversationJid.value = intent.getStringExtra(EXTRA_NAVIGATE_JID)
        // Consume-once: without this, every recreation (rotation, theme
        // change) replays the notification navigation from the sticky
        // launch intent.
        intent.removeExtra(EXTRA_NAVIGATE_JID)
        setContent {
            val themeMode by graph.userPrefs.theme
                .collectAsStateWithLifecycle(initialValue = ThemeMode.SYSTEM)
            WaddleTheme(themeMode) {
                CompositionLocalProvider(LocalAppGraph provides graph) {
                    AppShell(
                        pendingConversationJid = pendingConversationJid,
                        onConversationConsumed = { pendingConversationJid.value = null },
                    )
                }
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        intent.getStringExtra(EXTRA_NAVIGATE_JID)?.let { pendingConversationJid.value = it }
        intent.removeExtra(EXTRA_NAVIGATE_JID)
        setIntent(intent)
    }

    companion object {
        /** Notification taps carry the conversation bare JID to open. */
        const val EXTRA_NAVIGATE_JID = "waddle.navigate.jid"
    }
}

package social.waddle.android

import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.platform.LocalView
import androidx.core.splashscreen.SplashScreen.Companion.installSplashScreen
import androidx.core.view.WindowInsetsControllerCompat
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.lifecycleScope
import androidx.lifecycle.repeatOnLifecycle
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.drop
import kotlinx.coroutines.flow.filterNotNull
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch
import social.waddle.android.avatar.LocalPeerAvatars
import social.waddle.android.client.prefs.ThemeMode
import social.waddle.android.theme.WaddleTheme

/**
 * The single activity: edge-to-edge, splash held until the app state
 * leaves `Loading`, and `waddle.navigate.jid` intent extras (notification
 * taps) funneled into the app shell for navigation.
 */
class MainActivity : ComponentActivity() {
    private val pendingConversationJid = MutableStateFlow<String?>(null)

    /** The incoming-call notification's Answer action was tapped. */
    private val pendingCallAnswer = MutableStateFlow(false)

    override fun onCreate(savedInstanceState: Bundle?) {
        val splashScreen = installSplashScreen()
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        val graph = (application as WaddleApplication).graph
        splashScreen.setKeepOnScreenCondition {
            // Local init only — never the session-validation network call
            // (the in-app Loading composable covers that).
            graph.bootstrap.splashHold.value
        }
        // Fresh starts read the launch extra; recreations restore the
        // still-unconsumed value from the saved state instead. Gating on
        // savedInstanceState alone conflated two cases: a process-death
        // restore (already navigated — must NOT replay the stale extra)
        // and a config change during the post-tap Loading window (must
        // keep the pending target or the tap silently lands on Home).
        pendingConversationJid.value = if (savedInstanceState == null) {
            intent.getStringExtra(EXTRA_NAVIGATE_JID)
        } else {
            savedInstanceState.getString(STATE_PENDING_JID)
        }
        intent.removeExtra(EXTRA_NAVIGATE_JID)
        // Deliberately NOT restored across recreation: replaying a stale
        // answer after a config change could accept a later call.
        if (savedInstanceState == null && intent.getBooleanExtra(EXTRA_ANSWER_CALL, false)) {
            pendingCallAnswer.value = true
        }
        intent.removeExtra(EXTRA_ANSWER_CALL)
        watchPushKey(graph)
        setContent {
            val themeMode by graph.userPrefs.theme
                // Night until the stored choice loads: the window ground is
                // night too, so a night first frame never flashes.
                .collectAsStateWithLifecycle(initialValue = ThemeMode.NIGHT)
            // enableEdgeToEdge derives bar-icon contrast from the SYSTEM
            // night mode once at onCreate; the in-app theme is independent
            // of it, so a daylight app on a dark phone would get invisible
            // white status icons. Track the app's theme.
            val darkTheme = themeMode == ThemeMode.NIGHT
            val wallpaperColors by graph.userPrefs.wallpaperColors
                .collectAsStateWithLifecycle(initialValue = false)
            val view = LocalView.current
            SideEffect {
                val controller = WindowInsetsControllerCompat(window, view)
                controller.isAppearanceLightStatusBars = !darkTheme
                controller.isAppearanceLightNavigationBars = !darkTheme
            }
            WaddleTheme(themeMode, wallpaperColors = wallpaperColors) {
                CompositionLocalProvider(
                    LocalAppGraph provides graph,
                    LocalPeerAvatars provides graph.sessionManager.peerAvatars,
                ) {
                    AppShell(
                        pendingConversationJid = pendingConversationJid,
                        onConversationConsumed = { pendingConversationJid.value = null },
                        pendingCallAnswer = pendingCallAnswer,
                        onCallAnswerConsumed = { pendingCallAnswer.value = false },
                    )
                }
            }
        }
    }

    override fun onStart() {
        super.onStart()
        // Foreground is the one place a distributor may be chosen (it can
        // show UI); UnifiedPush also asks for a register on every start.
        (application as WaddleApplication).graph.pushDistributor.onForeground(this)
    }

    /**
     * The server key is only learned once a stream is ready, often after
     * onStart (first sign-in, sign-in after sign-out): run the foreground
     * registration again when it appears or changes while started.
     */
    private fun watchPushKey(graph: AppGraph) {
        lifecycleScope.launch {
            repeatOnLifecycle(Lifecycle.State.STARTED) {
                graph.sessionManager.pushState
                    .map { it?.advertised?.kid }
                    .distinctUntilChanged()
                    .drop(1)
                    .filterNotNull()
                    .collect { graph.pushDistributor.onForeground(this@MainActivity) }
            }
        }
    }

    override fun onSaveInstanceState(outState: Bundle) {
        super.onSaveInstanceState(outState)
        outState.putString(STATE_PENDING_JID, pendingConversationJid.value)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        intent.getStringExtra(EXTRA_NAVIGATE_JID)?.let { pendingConversationJid.value = it }
        intent.removeExtra(EXTRA_NAVIGATE_JID)
        if (intent.getBooleanExtra(EXTRA_ANSWER_CALL, false)) {
            pendingCallAnswer.value = true
        }
        intent.removeExtra(EXTRA_ANSWER_CALL)
        setIntent(intent)
    }

    companion object {
        /** Notification taps carry the conversation bare JID to open. */
        const val EXTRA_NAVIGATE_JID = "waddle.navigate.jid"

        /** Incoming-call Answer action: accept once the shell is up. */
        const val EXTRA_ANSWER_CALL = "waddle.call.answer"
        private const val STATE_PENDING_JID = "waddle.state.pending-jid"
    }
}

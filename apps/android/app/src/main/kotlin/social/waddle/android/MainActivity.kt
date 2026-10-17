package social.waddle.android

import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalView
import androidx.core.splashscreen.SplashScreen.Companion.installSplashScreen
import androidx.core.view.WindowInsetsControllerCompat
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleResumeEffect
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
import social.waddle.android.ui.huddle.LocalReducedMotion
import social.waddle.android.ui.huddle.reducedMotionOf

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
                // System until the stored choice loads: the window ground
                // follows the system too, so the first frame never flashes.
                .collectAsStateWithLifecycle(initialValue = ThemeMode.SYSTEM)
            // enableEdgeToEdge derives bar-icon contrast from the SYSTEM
            // night mode once at onCreate; a pinned in-app theme can differ
            // from it, so a daylight app on a dark phone would get invisible
            // white status icons. Track the app's theme.
            val darkTheme = themeMode.isNight(isSystemInDarkTheme())
            val wallpaperColors by graph.userPrefs.wallpaperColors
                .collectAsStateWithLifecycle(initialValue = false)
            val view = LocalView.current
            // An effect keyed on the theme, not a SideEffect: it runs only
            // when the theme changes and before any child's effect, so a
            // night call surface (LightSystemBarIcons) still wins after an
            // unrelated recomposition such as the wallpaper toggle.
            DisposableEffect(view, darkTheme) {
                val controller = WindowInsetsControllerCompat(window, view)
                controller.isAppearanceLightStatusBars = !darkTheme
                controller.isAppearanceLightNavigationBars = !darkTheme
                onDispose { }
            }
            WaddleTheme(themeMode, wallpaperColors = wallpaperColors) {
                CompositionLocalProvider(
                    LocalAppGraph provides graph,
                    LocalPeerAvatars provides graph.sessionManager.peerAvatars,
                    LocalReducedMotion provides rememberSystemReducedMotion(),
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
        // show UI); UnifiedPush also asks for a register on every start,
        // and FCM tokens are re-checked here.
        (application as WaddleApplication).graph.push.onForeground(this)
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
                    .collect { graph.push.onForeground(this@MainActivity) }
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

/**
 * Android's "Remove animations" (animator duration scale 0), re-read on
 * every resume: the user flips it in system settings, outside the app.
 */
@Composable
private fun rememberSystemReducedMotion(): Boolean {
    val context = LocalContext.current
    var reduced by remember { mutableStateOf(reducedMotionOf(context)) }
    LifecycleResumeEffect(context) {
        reduced = reducedMotionOf(context)
        onPauseOrDispose { }
    }
    return reduced
}

package social.waddle.android

import android.Manifest
import android.content.pm.PackageManager
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.StartOffset
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.systemBarsPadding
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.CloudOff
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import social.waddle.android.client.WaddleAppState
import social.waddle.android.feature.call.CallOverlayHost
import social.waddle.android.feature.login.LoginScreen
import social.waddle.android.navigation.WaddleNavDisplay
import social.waddle.android.navigation.WaddleNavKey
import social.waddle.android.navigation.conversationNavKeyFor
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.EmptyState
import social.waddle.android.ui.EmptyStateTone
import social.waddle.android.ui.Wordmark
import social.waddle.android.ui.huddle.HuddleMotion
import social.waddle.android.ui.huddle.PillButton
import social.waddle.android.ui.huddle.reducedMotion

/**
 * App-state switch (web `AppShell.vue` parity): Loading → progress,
 * SignedOut → login, Error → retry, Ready → the navigation shell.
 */
@Composable
fun AppShell(
    pendingConversationJid: StateFlow<String?>,
    onConversationConsumed: () -> Unit,
    pendingCallAnswer: StateFlow<Boolean> = MutableStateFlow(false),
    onCallAnswerConsumed: () -> Unit = {},
) {
    val graph = LocalAppGraph.current
    val appState by graph.appState.collectAsStateWithLifecycle()
    Surface(
        modifier = Modifier.fillMaxSize().testTag(AppShellTestTags.ROOT),
        color = Waddle.colors.bg,
    ) {
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
                pendingCallAnswer = pendingCallAnswer,
                onCallAnswerConsumed = onCallAnswerConsumed,
            )
        }
    }
}

/**
 * App startup (web `AppShell` loading): the wordmark, "Checking
 * session." and three teal typing dots. The dots hold still under
 * reduced motion.
 */
@Composable
internal fun LoadingScreen() {
    val description = stringResource(R.string.huddle_app_checking_session)
    Column(
        modifier = Modifier
            .fillMaxSize()
            .background(Waddle.colors.bg)
            .semantics(mergeDescendants = true) { contentDescription = description },
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Wordmark(style = Waddle.type.headline)
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs),
            modifier = Modifier.padding(top = Waddle.space.md),
        ) {
            TypingDots()
            Text(text = description, style = Waddle.type.control, color = Waddle.colors.fgMuted)
        }
    }
}

/** Three teal dots breathing in turn (web typing dots: .85 → 1.15 over 1.25s). */
@Composable
private fun TypingDots() {
    val color = Waddle.colors.action
    val scales = if (reducedMotion) {
        List(DOT_COUNT) { 1f }
    } else {
        val transition = rememberInfiniteTransition(label = "checking")
        List(DOT_COUNT) { dot ->
            transition.animateFloat(
                initialValue = DOT_MIN_SCALE,
                targetValue = DOT_MAX_SCALE,
                animationSpec = infiniteRepeatable(
                    animation = tween(DOT_CYCLE_MILLIS / 2, easing = HuddleMotion.easing),
                    repeatMode = RepeatMode.Reverse,
                    initialStartOffset = StartOffset(dot * DOT_STAGGER_MILLIS),
                ),
                label = "dot$dot",
            ).value
        }
    }
    Row(horizontalArrangement = Arrangement.spacedBy(Waddle.space.xxs)) {
        scales.forEach { scale ->
            Box(
                modifier = Modifier
                    .size(TYPING_DOT)
                    .graphicsLayer {
                        scaleX = scale
                        scaleY = scale
                    }
                    .background(color, CircleShape),
            )
        }
    }
}

/** The session could not be restored: say so kindly, offer the retry, keep the detail small. */
@Composable
internal fun RestoreErrorScreen(detail: String, onRetry: () -> Unit) {
    EmptyState(
        icon = Icons.Outlined.CloudOff,
        title = stringResource(R.string.restore_error_title),
        body = stringResource(R.string.huddle_app_restore_body),
        tone = EmptyStateTone.PROBLEM,
        modifier = Modifier
            .background(Waddle.colors.bg)
            .systemBarsPadding(),
        action = {
            Column(
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(Waddle.space.md),
            ) {
                PillButton(text = stringResource(R.string.action_retry), onClick = onRetry)
                if (detail.isNotBlank()) {
                    Text(
                        text = detail,
                        style = Waddle.type.time,
                        color = Waddle.colors.fgSoft,
                        textAlign = TextAlign.Center,
                        maxLines = DETAIL_LINES,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
        },
    )
}

private const val DOT_COUNT = 3
private const val DOT_MIN_SCALE = 0.85f
private const val DOT_MAX_SCALE = 1.15f
private const val DOT_CYCLE_MILLIS = 1250
private const val DOT_STAGGER_MILLIS = 180
private const val DETAIL_LINES = 3
private val TYPING_DOT = 6.dp

@Composable
private fun ReadyShell(
    pendingConversationJid: StateFlow<String?>,
    onConversationConsumed: () -> Unit,
    pendingCallAnswer: StateFlow<Boolean>,
    onCallAnswerConsumed: () -> Unit,
) {
    val graph = LocalAppGraph.current
    RequestNotificationPermissionOnce()

    // Call surfaces wrap whatever screen is open: the ring and the
    // in-call screen layer over it; the minimized and ended banners take
    // a row above it so they never cover a top bar.
    CallOverlayHost(
        pendingCallAnswer = pendingCallAnswer,
        onCallAnswerConsumed = onCallAnswerConsumed,
    ) {
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
                // MUC destinations (channels AND group DMs) join ahead
                // of the screen so the timeline is live on arrival.
                val mucRoomJid = when (key) {
                    is WaddleNavKey.Channel -> key.roomJid
                    is WaddleNavKey.GroupDm -> key.roomJid
                    else -> null
                }
                if (mucRoomJid != null) {
                    graph.sessionManager.joinRoom(
                        roomJid = mucRoomJid,
                        nick = graph.currentSession.value?.xmppLocalpart ?: DEFAULT_NICK,
                    )
                }
                onConversationConsumed()
            }
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

/** Semantics tags shared with instrumented tests. */
object AppShellTestTags {
    const val ROOT = "waddle-app-shell"
}

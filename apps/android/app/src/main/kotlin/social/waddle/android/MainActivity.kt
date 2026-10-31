package social.waddle.android

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Bundle
import android.view.View
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.core.content.ContextCompat
import androidx.core.splashscreen.SplashScreen.Companion.installSplashScreen
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.lifecycleScope
import androidx.lifecycle.repeatOnLifecycle
import io.flutter.embedding.android.FlutterFragmentActivity
import io.flutter.embedding.engine.FlutterEngine
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.drop
import kotlinx.coroutines.flow.filterNotNull
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch
import social.waddle.android.client.WaddleAppState
import social.waddle.android.client.calls.CallState
import social.waddle.android.flutter.FlutterBridge
import social.waddle.android.flutter.FlutterCallVideoFactory

/** Flutter presentation host; the application graph continues to own the native session. */
class MainActivity : FlutterFragmentActivity() {
    private val pendingConversationJid = MutableStateFlow<String?>(null)
    private val pendingCallSid = MutableStateFlow<String?>(null)
    private var bridge: FlutterBridge? = null
    private var notificationPermissionRequested = false
    private val notificationPermission =
        registerForActivityResult(ActivityResultContracts.RequestPermission()) {}
    private val graph: AppGraph
        get() = (application as WaddleApplication).graph

    override fun onCreate(savedInstanceState: Bundle?) {
        val splashScreen = installSplashScreen()
        // configureFlutterEngine runs inside super.onCreate. Read launch state
        // first so neither engine setup nor a fast Dart subscriber loses it.
        pendingConversationJid.value =
            if (savedInstanceState == null) {
                intent.getStringExtra(EXTRA_NAVIGATE_JID)
            } else {
                savedInstanceState.getString(STATE_PENDING_JID)
            }
        notificationPermissionRequested =
            savedInstanceState?.getBoolean(STATE_NOTIFICATION_PERMISSION) ?: false
        consumeCallAnswer(intent, restored = savedInstanceState != null)
        intent.removeExtra(EXTRA_NAVIGATE_JID)
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        findViewById<View>(android.R.id.content).setOnDragListener { _, event ->
            bridge?.handleDrop(event) ?: false
        }
        // Network session validation is represented by Flutter's loading state;
        // it must never hold Android's launch splash indefinitely.
        splashScreen.setKeepOnScreenCondition { graph.bootstrap.splashHold.value }
        watchPushKey()
        dispatchPendingIntents()
    }

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        bridge = FlutterBridge(this, graph, flutterEngine.dartExecutor.binaryMessenger)
        flutterEngine.platformViewsController.registry.registerViewFactory(
            "social.waddle.android/call-video",
            FlutterCallVideoFactory(graph, graph.applicationScope),
        )
    }

    override fun cleanUpFlutterEngine(flutterEngine: FlutterEngine) {
        bridge?.close()
        bridge = null
        super.cleanUpFlutterEngine(flutterEngine)
    }

    override fun onStart() {
        super.onStart()
        bridge?.setVisible(true)
        // UnifiedPush distributor selection may display UI, so it remains a
        // foreground activity action. FCM tokens are rechecked here as well.
        graph.push.onForeground(this)
    }

    override fun onStop() {
        bridge?.setVisible(false)
        super.onStop()
    }

    private fun watchPushKey() {
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

    private fun dispatchPendingIntents() {
        lifecycleScope.launch {
            repeatOnLifecycle(Lifecycle.State.STARTED) {
                combine(graph.appState, pendingConversationJid, pendingCallSid) { state, jid, sid ->
                        Triple(state, jid, sid)
                    }
                    .collect { (state, jid, sid) ->
                        if (state != WaddleAppState.Ready) return@collect
                        requestNotificationPermissionOnce()
                        val currentBridge = bridge ?: return@collect
                        jid?.let {
                            // The bridge retains this until Dart subscribes. Save
                            // that queue too if Android recreates us meanwhile.
                            currentBridge.navigate(it)
                            pendingConversationJid.value = null
                        }
                        sid?.let {
                            // A stale notification must not accept a later call.
                            if (
                                (graph.sessionManager.callStore.state.value as? CallState.Incoming)
                                    ?.sid == it
                            ) {
                                currentBridge.answerPendingCall()
                            }
                            pendingCallSid.value = null
                        }
                    }
            }
        }
    }

    private fun requestNotificationPermissionOnce() {
        if (notificationPermissionRequested) return
        notificationPermissionRequested = true
        if (
            ContextCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS) !=
                PackageManager.PERMISSION_GRANTED
        ) {
            notificationPermission.launch(Manifest.permission.POST_NOTIFICATIONS)
        }
    }

    override fun onSaveInstanceState(outState: Bundle) {
        outState.putString(
            STATE_PENDING_JID,
            pendingConversationJid.value ?: bridge?.pendingNavigationJid,
        )
        outState.putBoolean(STATE_NOTIFICATION_PERMISSION, notificationPermissionRequested)
        super.onSaveInstanceState(outState)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        intent.getStringExtra(EXTRA_NAVIGATE_JID)?.let { pendingConversationJid.value = it }
        intent.removeExtra(EXTRA_NAVIGATE_JID)
        consumeCallAnswer(intent, restored = false)
        setIntent(intent)
    }

    private fun consumeCallAnswer(intent: Intent, restored: Boolean) {
        if (!restored && intent.getBooleanExtra(EXTRA_ANSWER_CALL, false)) {
            pendingCallSid.value =
                (graph.sessionManager.callStore.state.value as? CallState.Incoming)?.sid
        }
        // Call acceptance is deliberately not restored across recreation.
        intent.removeExtra(EXTRA_ANSWER_CALL)
    }

    companion object {
        const val EXTRA_NAVIGATE_JID = "waddle.navigate.jid"
        const val EXTRA_ANSWER_CALL = "waddle.call.answer"
        private const val STATE_PENDING_JID = "waddle.state.pending-jid"
        private const val STATE_NOTIFICATION_PERMISSION = "waddle.state.notification-permission"
    }
}

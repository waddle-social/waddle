package social.waddle.android.service

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import kotlinx.coroutines.launch
import social.waddle.android.WaddleApplication

/**
 * Notification call actions that never need UI: decline an incoming
 * ring, hang up the live call. Both send their XEP-0353/0166 stanzas
 * through the session manager directly — no activity trampoline
 * (Android 12+ rules). Answering DOES go through the activity because
 * accepting requires the RECORD_AUDIO runtime prompt.
 */
class CallActionReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val action = intent.action ?: return
        if (action != ACTION_DECLINE && action != ACTION_HANG_UP) return
        val graph = (context.applicationContext as WaddleApplication).graph
        val pendingResult = goAsync()
        graph.applicationScope.launch {
            try {
                runCatching {
                    when (action) {
                        ACTION_DECLINE -> graph.sessionManager.callStore.declineIncoming()
                        ACTION_HANG_UP -> graph.sessionManager.callStore.hangUp()
                    }
                }
            } finally {
                pendingResult.finish()
            }
        }
    }

    companion object {
        const val ACTION_DECLINE = "social.waddle.android.action.CALL_DECLINE"
        const val ACTION_HANG_UP = "social.waddle.android.action.CALL_HANG_UP"
    }
}

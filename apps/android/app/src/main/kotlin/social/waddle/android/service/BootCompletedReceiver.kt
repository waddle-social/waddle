package social.waddle.android.service

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import social.waddle.android.WaddleApplication

/**
 * Boot restart: if a session is persisted, raise the connection service
 * (specialUse may start from BOOT_COMPLETED). The Application init that
 * runs alongside this receiver performs the actual session restore.
 */
class BootCompletedReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != Intent.ACTION_BOOT_COMPLETED) return
        val graph = (context.applicationContext as WaddleApplication).graph
        val pendingResult = goAsync()
        graph.applicationScope.launch {
            try {
                if (graph.sessionPrefs.sessionId.first() != null) {
                    graph.serviceController.startService()
                }
            } finally {
                pendingResult.finish()
            }
        }
    }
}

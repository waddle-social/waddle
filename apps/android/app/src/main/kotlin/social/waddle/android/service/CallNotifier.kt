package social.waddle.android.service

import android.Manifest
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.app.Person
import androidx.core.content.ContextCompat
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import social.waddle.android.MainActivity
import social.waddle.android.R
import social.waddle.android.client.calls.CallState
import social.waddle.android.jid.bareJidOf
import social.waddle.android.jid.localpartOf

/**
 * Incoming-call ring notification: a CallStyle notification with a
 * full-screen intent plus answer/decline actions, posted while the
 * call slot is `Incoming` and retired the moment it leaves — the
 * XEP-0353 retract/reject/timeout paths all collapse to that
 * transition. Sibling of [MessageNotifier], but driven by call state
 * instead of the message stream.
 */
class CallNotifier(
    private val context: Context,
    private val callState: StateFlow<CallState>,
) {
    private var ringingSid: String? = null

    fun start(scope: CoroutineScope) {
        // A process death mid-ring strands an ongoing (non-swipeable)
        // ring notification whose actions target a dead slot — retire
        // it unconditionally before observing the fresh state.
        runCatching {
            NotificationManagerCompat.from(context).cancel(INCOMING_CALL_NOTIFICATION_ID)
        }
        scope.launch {
            callState.collect { state ->
                // Per-event guard: notify() can throw at runtime; an
                // uncaught throw would kill the collector and all
                // future call notifications.
                runCatching { onCallState(state) }
            }
        }
    }

    private fun onCallState(state: CallState) {
        val incoming = state as? CallState.Incoming
        if (incoming == null || incoming.accepting) {
            if (ringingSid != null) {
                ringingSid = null
                NotificationManagerCompat.from(context).cancel(INCOMING_CALL_NOTIFICATION_ID)
            }
            return
        }
        if (ringingSid == incoming.sid) return
        ringingSid = incoming.sid
        postIncoming(incoming)
    }

    private fun postIncoming(state: CallState.Incoming) {
        val granted = ContextCompat.checkSelfPermission(
            context,
            Manifest.permission.POST_NOTIFICATIONS,
        ) == PackageManager.PERMISSION_GRANTED
        if (!granted) return

        val peerBare = bareJidOf(state.from)
        val caller = Person.Builder()
            .setName(localpartOf(peerBare))
            .setKey(peerBare)
            .setImportant(true)
            .build()
        val fullScreen = activityIntent(answer = false)
        val answer = activityIntent(answer = true)
        val decline = PendingIntent.getBroadcast(
            context,
            0,
            Intent(context, CallActionReceiver::class.java)
                .setAction(CallActionReceiver.ACTION_DECLINE),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val notification = NotificationCompat.Builder(context, NotificationChannels.INCOMING_CALLS)
            .setSmallIcon(R.drawable.ic_launcher_foreground)
            .setContentTitle(localpartOf(peerBare))
            .setContentText(
                context.getString(
                    if (state.media.video) R.string.call_incoming_video else R.string.call_incoming_audio,
                ),
            )
            .setStyle(NotificationCompat.CallStyle.forIncomingCall(caller, decline, answer))
            .setCategory(NotificationCompat.CATEGORY_CALL)
            .setPriority(NotificationCompat.PRIORITY_MAX)
            .setOngoing(true)
            // USE_FULL_SCREEN_INTENT is user-revocable on 34+; the
            // system degrades to a heads-up notification when denied.
            .setFullScreenIntent(fullScreen, true)
            .setContentIntent(fullScreen)
            .build()
        NotificationManagerCompat.from(context)
            .notify(INCOMING_CALL_NOTIFICATION_ID, notification)
    }

    private fun activityIntent(answer: Boolean): PendingIntent {
        val intent = Intent(context, MainActivity::class.java)
            .setData(android.net.Uri.fromParts("waddle-call", if (answer) "answer" else "open", null))
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        if (answer) intent.putExtra(MainActivity.EXTRA_ANSWER_CALL, true)
        return PendingIntent.getActivity(
            context,
            0,
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
    }

    private companion object {
        /** Distinct from connection (1), messages (2), in-call FGS (3). */
        const val INCOMING_CALL_NOTIFICATION_ID = 4
    }
}

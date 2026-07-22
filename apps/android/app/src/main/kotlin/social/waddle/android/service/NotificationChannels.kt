package social.waddle.android.service

import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.Context
import android.media.AudioAttributes
import android.media.RingtoneManager
import social.waddle.android.R

/**
 * Notification channels: silent connection status, messages, the
 * ringing incoming-call channel, and the silent ongoing-call channel
 * backing the in-call foreground service.
 */
object NotificationChannels {
    const val CONNECTION = "connection"
    const val MESSAGES = "messages"
    const val INCOMING_CALLS = "incoming_calls"
    const val ONGOING_CALLS = "ongoing_calls"

    fun ensure(context: Context) {
        val manager = context.getSystemService(NotificationManager::class.java) ?: return
        val connection = NotificationChannel(
            CONNECTION,
            context.getString(R.string.notification_channel_connection),
            NotificationManager.IMPORTANCE_MIN,
        ).apply {
            description = context.getString(R.string.notification_channel_connection_description)
            setShowBadge(false)
            setSound(null, null)
            enableVibration(false)
        }
        val messages = NotificationChannel(
            MESSAGES,
            context.getString(R.string.notification_channel_messages),
            NotificationManager.IMPORTANCE_HIGH,
        ).apply {
            description = context.getString(R.string.notification_channel_messages_description)
        }
        val incomingCalls = NotificationChannel(
            INCOMING_CALLS,
            context.getString(R.string.notification_channel_incoming_calls),
            NotificationManager.IMPORTANCE_HIGH,
        ).apply {
            description = context.getString(R.string.notification_channel_incoming_calls_description)
            setSound(
                RingtoneManager.getDefaultUri(RingtoneManager.TYPE_RINGTONE),
                AudioAttributes.Builder()
                    .setUsage(AudioAttributes.USAGE_NOTIFICATION_RINGTONE)
                    .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
                    .build(),
            )
            enableVibration(true)
        }
        val ongoingCalls = NotificationChannel(
            ONGOING_CALLS,
            context.getString(R.string.notification_channel_ongoing_calls),
            NotificationManager.IMPORTANCE_LOW,
        ).apply {
            description = context.getString(R.string.notification_channel_ongoing_calls_description)
            setShowBadge(false)
            setSound(null, null)
            enableVibration(false)
        }
        manager.createNotificationChannels(listOf(connection, messages, incomingCalls, ongoingCalls))
    }
}

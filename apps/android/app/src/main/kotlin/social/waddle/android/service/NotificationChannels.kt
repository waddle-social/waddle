package social.waddle.android.service

import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.Context
import social.waddle.android.R

/** The two notification channels: silent connection status + messages. */
object NotificationChannels {
    const val CONNECTION = "connection"
    const val MESSAGES = "messages"

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
        manager.createNotificationChannels(listOf(connection, messages))
    }
}

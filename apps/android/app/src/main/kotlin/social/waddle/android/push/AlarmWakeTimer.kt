package social.waddle.android.push

import android.app.AlarmManager
import android.content.Context
import android.os.Handler
import android.os.Looper

/**
 * [WakeTimer] on an inexact `ELAPSED_REALTIME_WAKEUP` alarm: elapsed
 * realtime keeps counting in deep sleep, and the alarm wakes the device,
 * so a locked phone still steps through auto-away. Doze may hold it until
 * the next maintenance window; the evaluation it triggers measures against
 * elapsed realtime, so a late step catches up at once. A listener alarm
 * needs no exported receiver and dies with the process, which is fine:
 * only a live process holds a stream to advertise presence on.
 */
class AlarmWakeTimer(context: Context) : WakeTimer {
    private val alarms: AlarmManager = context.getSystemService(AlarmManager::class.java)
    private val handler = Handler(Looper.getMainLooper())
    private val lock = Any()
    private var pending: AlarmManager.OnAlarmListener? = null

    override fun schedule(atElapsedRealtimeMillis: Long, onFire: () -> Unit) {
        val listener = AlarmManager.OnAlarmListener { onFire() }
        synchronized(lock) {
            pending?.let(alarms::cancel)
            pending = listener
            alarms.set(AlarmManager.ELAPSED_REALTIME_WAKEUP, atElapsedRealtimeMillis, TAG, listener, handler)
        }
    }

    override fun cancel() {
        synchronized(lock) {
            pending?.let(alarms::cancel)
            pending = null
        }
    }

    private companion object {
        const val TAG = "waddle:auto-away"
    }
}

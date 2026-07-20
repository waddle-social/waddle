package social.waddle.android.client

import social.waddle.android.client.prefs.ProcessEpoch

/** One immutable epoch for this Android process; tests inject replacement epochs at worker construction. */
internal object DeliveryProcessEpoch {
    val current: ProcessEpoch = ProcessEpoch.random()
}

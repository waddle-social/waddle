package social.waddle.android.client

import social.waddle.android.client.prefs.ProcessEpoch

/**
 * One immutable epoch for this Android process. Android uses one process for
 * the preferencesDataStore owner (no secondary android:process declaration),
 * so a different production epoch means the previous process has died.
 * Tests inject replacement epochs at worker construction to model that restart.
 */
internal object DeliveryProcessEpoch {
    val current: ProcessEpoch = ProcessEpoch.random()
}

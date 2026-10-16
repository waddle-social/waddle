package social.waddle.android.client

import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Test
import social.waddle.android.client.prefs.PushDevice
import social.waddle.android.client.prefs.PushDeviceRef
import social.waddle.android.client.prefs.PushEndpoint
import social.waddle.android.client.prefs.PushState
import social.waddle.android.client.prefs.SessionPrefs

/**
 * The persisted push state survives the move to a sealed endpoint: an
 * install that registered over UnifiedPush before FCM existed keeps its
 * device and its queued disables, so it never registers twice.
 */
class PushStateStoreTest {
    @Test
    fun `state written before fcm existed still reads its web push endpoint and device`() = runTest {
        val dataStore = InMemoryPreferencesDataStore()
        dataStore.edit { it[stringPreferencesKey("push_state")] = LEGACY_STATE }
        val store = SessionPrefs(dataStore).push

        val state = store.read(OWNER)

        val endpoint = PushEndpoint.WebPush(
            url = "https://push.example/up/1",
            p256dh = "p256dh-1",
            auth = "auth-1",
            vapidKid = "kid-1",
        )
        assertEquals(endpoint, state.endpoint)
        val device = PushDevice(PushDeviceRef("push.waddle.test", "node-1", "dev-1"), endpoint, enabled = true)
        assertEquals(device, state.device)
        assertEquals(listOf(PushDeviceRef("push.waddle.test", "node-0", "dev-0")), state.pendingDisables)
    }

    @Test
    fun `an fcm endpoint round-trips and its string form leaks no secret`() = runTest {
        val store = SessionPrefs(InMemoryPreferencesDataStore()).push
        val endpoint = PushEndpoint.Fcm(
            token = "fcm-token-secret",
            p256dh = "p256dh-1",
            auth = "auth-secret",
            wrappedPrivateKey = "wrapped-secret",
        )

        store.update(OWNER) { it.copy(endpoint = endpoint) }

        assertEquals(PushState(owner = OWNER, endpoint = endpoint), store.read(OWNER))
        val shown = endpoint.toString()
        listOf("fcm-token-secret", "auth-secret", "wrapped-secret").forEach { secret ->
            assertFalse(shown, shown.contains(secret))
        }
    }

    private companion object {
        const val OWNER = "icepuma@waddle.test"
        val LEGACY_STATE = """
            {"owner":"$OWNER","advertised":{"publicKey":"BKEY","kid":"kid-1"},
             "endpoint":{"url":"https://push.example/up/1","p256dh":"p256dh-1","auth":"auth-1","vapidKid":"kid-1"},
             "device":{"ref":{"serviceJid":"push.waddle.test","node":"node-1","deviceId":"dev-1"},
                       "endpoint":{"url":"https://push.example/up/1","p256dh":"p256dh-1","auth":"auth-1","vapidKid":"kid-1"},
                       "enabled":true},
             "pendingDisables":[{"serviceJid":"push.waddle.test","node":"node-0","deviceId":"dev-0"}]}
        """.trimIndent()
    }
}

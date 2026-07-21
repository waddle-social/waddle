package social.waddle.android.client

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.store.TimelineStore
import java.lang.reflect.Modifier

class OutboundServiceArchitectureTest {
    @Test
    fun `send service retains only journal and timeline dependencies`() {
        assertDependencies(
            type = OutboundSendService::class.java,
            expected = mapOf(
                "journal" to DeliveryJournalStore::class.java,
                "timelineStore" to TimelineStore::class.java,
            ),
        )
    }

    @Test
    fun `drain service retains only journal and send service dependencies`() {
        assertDependencies(
            type = OutboundDrainService::class.java,
            expected = mapOf(
                "journal" to DeliveryJournalStore::class.java,
                "sendService" to OutboundSendService::class.java,
            ),
        )
    }

    private fun assertDependencies(type: Class<*>, expected: Map<String, Class<*>>) {
        val fields = type.declaredFields.filterNot { it.isSynthetic }
        assertEquals(expected.keys, fields.map { it.name }.toSet())
        fields.forEach { field ->
            assertEquals(expected.getValue(field.name), field.type)
            assertTrue(Modifier.isFinal(field.modifiers))
            assertFalse(field.type.name.matches(Regex(".*(Function|Mutex|Job|Channel|Atomic|Lifecycle|ActiveSession|SessionStores|ResumePersistence|Worker|Evidence).*")))
        }
    }
}

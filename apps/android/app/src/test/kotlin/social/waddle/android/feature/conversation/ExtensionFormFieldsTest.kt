package social.waddle.android.feature.conversation

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import social.waddle.android.client.ExtensionCommandField
import social.waddle.android.client.ExtensionFieldType

/** Web `form-fields.ts` / `ExtensionPalette.vue` gating parity. */
class ExtensionFormFieldsTest {
    private fun field(
        name: String,
        type: ExtensionFieldType = ExtensionFieldType.TEXT_SINGLE,
        required: Boolean = false,
        values: List<String> = emptyList(),
        blocked: Boolean = false,
        label: String? = null,
    ) = ExtensionCommandField(
        name = name,
        label = label,
        type = type,
        required = required,
        options = emptyList(),
        values = values,
        blocked = blocked,
    )

    @Test
    fun `blocked reason uses the web message with the label or name`() {
        assertNull(extensionFormBlockedReason(listOf(field("prompt"))))
        assertEquals(
            "Extension command form contains a forbidden field: API key.",
            extensionFormBlockedReason(
                listOf(field("payload#api_key", blocked = true, label = "API key")),
            ),
        )
        assertEquals(
            "Extension command form contains a forbidden field: password.",
            extensionFormBlockedReason(listOf(field("password", blocked = true))),
        )
    }

    @Test
    fun `missing required ignores hidden and fixed fields`() {
        val fields = listOf(
            field("kind", type = ExtensionFieldType.HIDDEN, required = true),
            field("note", type = ExtensionFieldType.FIXED, required = true),
            field("question", required = true),
        )
        assertEquals(
            listOf("question"),
            missingRequiredExtensionFields(fields).map { it.name },
        )
    }

    @Test
    fun `required multi fields need one non-blank value and singles a non-blank first`() {
        val satisfied = listOf(
            field("tags", type = ExtensionFieldType.LIST_MULTI, required = true, values = listOf("", "a")),
            field("question", required = true, values = listOf("Lunch?")),
        )
        assertEquals(emptyList<String>(), missingRequiredExtensionFields(satisfied).map { it.name })

        val missing = listOf(
            field("tags", type = ExtensionFieldType.TEXT_MULTI, required = true, values = listOf(" ")),
            field("question", required = true, values = listOf("")),
        )
        assertEquals(
            listOf("tags", "question"),
            missingRequiredExtensionFields(missing).map { it.name },
        )
    }
}

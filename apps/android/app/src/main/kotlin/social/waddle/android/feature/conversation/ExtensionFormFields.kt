package social.waddle.android.feature.conversation

import social.waddle.android.client.ExtensionCommandField
import social.waddle.android.client.ExtensionFieldType

/**
 * Pure form-gating helpers ported from the wasm chat client's
 * `form-fields.ts` / `ExtensionPalette.vue`: a form containing a
 * forbidden (secret-harvesting) field never submits, and `complete` /
 * `next` stay disabled until every visible required field has a value.
 */

/** First forbidden field of the form, `null` when the form is safe. */
fun extensionFormBlockedField(fields: List<ExtensionCommandField>): ExtensionCommandField? =
    fields.firstOrNull { it.blocked }

/**
 * Web `extensionCommandFormBlockedReason` parity, including the exact
 * message text.
 */
fun extensionFormBlockedReason(fields: List<ExtensionCommandField>): String? =
    extensionFormBlockedField(fields)?.let { field ->
        "Extension command form contains a forbidden field: ${field.label ?: field.name}."
    }

/** Web `hasRequiredValue` parity. */
private fun hasRequiredValue(field: ExtensionCommandField): Boolean = when (field.type) {
    ExtensionFieldType.FIXED -> true
    ExtensionFieldType.LIST_MULTI,
    ExtensionFieldType.TEXT_MULTI,
    ExtensionFieldType.JID_MULTI,
    -> field.values.any { it.isNotBlank() }
    else -> field.values.firstOrNull()?.isNotBlank() == true
}

/** Visible required fields still missing a value (web parity). */
fun missingRequiredExtensionFields(
    fields: List<ExtensionCommandField>,
): List<ExtensionCommandField> = fields.filter { field ->
    field.type != ExtensionFieldType.HIDDEN && field.required && !hasRequiredValue(field)
}

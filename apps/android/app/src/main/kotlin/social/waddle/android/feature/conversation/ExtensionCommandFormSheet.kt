package social.waddle.android.feature.conversation

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Checkbox
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import social.waddle.android.R
import social.waddle.android.client.ExtensionCommandAction
import social.waddle.android.client.ExtensionCommandField
import social.waddle.android.client.ExtensionFieldType
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.huddle.PillButton
import social.waddle.android.ui.huddle.PillTone

/**
 * The slash-command palette: one XEP-0004 form stage of an extension
 * command rendered as a bottom sheet. Hidden and fixed fields are
 * skipped visually (they still submit); the action row offers exactly
 * the XEP-0050 actions the response advertised. Warning/error phases
 * (including server `<note/>` details) surface inside the sheet.
 *
 * Web-parity gating: a form containing a forbidden field disables all
 * inputs and shows the blocked reason, and `next`/`complete` stay
 * disabled while the form is blocked or any visible required field is
 * empty (`cancel`/`prev` always work).
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun ExtensionCommandFormSheet(
    session: ExtensionFormSession,
    phase: CommandPhase?,
    onFieldChange: (name: String, values: List<String>) -> Unit,
    onAction: (ExtensionCommandAction) -> Unit,
    onDismiss: () -> Unit,
) {
    ConversationSheet(onDismissRequest = onDismiss, title = session.title ?: session.command.name) {
        Column(
            verticalArrangement = Arrangement.spacedBy(Waddle.space.sm),
            modifier = Modifier
                .verticalScroll(rememberScrollState())
                .testTag(SlashCommandTestTags.FORM_SHEET),
        ) {
            session.instructions?.let { instructions ->
                Text(text = instructions, style = Waddle.type.control, color = Waddle.colors.fgMuted)
            }
            phase?.detail?.takeIf { phase.state != CommandPhaseState.LOADING }?.let { detail ->
                Text(
                    text = detail,
                    style = Waddle.type.control,
                    color = if (phase.state == CommandPhaseState.ERROR) {
                        Waddle.colors.dangerText
                    } else {
                        Waddle.colors.warningText
                    },
                )
            }
            val blockedReason = extensionFormBlockedReason(session.fields)
            val missingRequired = missingRequiredExtensionFields(session.fields)
            if (blockedReason != null) {
                Text(
                    text = blockedReason,
                    style = Waddle.type.control,
                    color = Waddle.colors.dangerText,
                    modifier = Modifier.testTag(SlashCommandTestTags.FORM_BLOCKED),
                )
            } else if (missingRequired.isNotEmpty()) {
                Text(
                    text = stringResource(R.string.extension_form_required_hint),
                    style = MetaStyle,
                    color = Waddle.colors.fgMuted,
                )
            }
            session.fields
                .filterNot { it.type == ExtensionFieldType.HIDDEN || it.type == ExtensionFieldType.FIXED }
                .forEach { field ->
                    FormField(
                        field = field,
                        enabled = blockedReason == null,
                        onFieldChange = onFieldChange,
                    )
                }
            ActionRow(
                actions = session.actions,
                busy = phase?.state == CommandPhaseState.LOADING,
                submitAllowed = blockedReason == null && missingRequired.isEmpty(),
                onAction = onAction,
            )
        }
    }
}

@Composable
private fun FormField(
    field: ExtensionCommandField,
    enabled: Boolean,
    onFieldChange: (name: String, values: List<String>) -> Unit,
) {
    val label = fieldLabel(field)
    when (field.type) {
        ExtensionFieldType.BOOLEAN -> {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier
                    .fillMaxWidth()
                    .heightIn(min = Waddle.space.xxl),
            ) {
                Text(
                    text = label,
                    style = Waddle.type.control,
                    modifier = Modifier.weight(1f),
                )
                val checked = field.values.firstOrNull() == "1" || field.values.firstOrNull() == "true"
                Switch(
                    checked = checked,
                    enabled = enabled,
                    onCheckedChange = { onFieldChange(field.name, listOf(if (it) "true" else "false")) },
                )
            }
        }
        ExtensionFieldType.LIST_SINGLE -> {
            Column {
                Text(text = label, style = Waddle.type.kicker)
                field.options.forEach { option ->
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        modifier = Modifier
                            .fillMaxWidth()
                            .heightIn(min = Waddle.space.xxl)
                            .clickable(enabled = enabled) {
                                onFieldChange(field.name, listOf(option.value))
                            },
                    ) {
                        RadioButton(
                            selected = field.values.firstOrNull() == option.value,
                            enabled = enabled,
                            onClick = { onFieldChange(field.name, listOf(option.value)) },
                        )
                        Text(
                            text = option.label ?: option.value,
                            style = Waddle.type.control,
                        )
                    }
                }
            }
        }
        ExtensionFieldType.LIST_MULTI -> {
            Column {
                Text(text = label, style = Waddle.type.kicker)
                field.options.forEach { option ->
                    val selected = option.value in field.values
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        modifier = Modifier
                            .fillMaxWidth()
                            .heightIn(min = Waddle.space.xxl)
                            .clickable(enabled = enabled) {
                                onFieldChange(field.name, toggleValue(field.values, option.value))
                            },
                    ) {
                        Checkbox(
                            checked = selected,
                            enabled = enabled,
                            onCheckedChange = {
                                onFieldChange(field.name, toggleValue(field.values, option.value))
                            },
                        )
                        Text(
                            text = option.label ?: option.value,
                            style = Waddle.type.control,
                        )
                    }
                }
            }
        }
        ExtensionFieldType.TEXT_MULTI -> OutlinedTextField(
            value = field.values.joinToString("\n"),
            onValueChange = { onFieldChange(field.name, it.split("\n")) },
            label = { Text(label) },
            enabled = enabled,
            minLines = 3,
            shape = RoundedCornerShape(Waddle.radius.control),
            modifier = Modifier.fillMaxWidth(),
        )
        else -> OutlinedTextField(
            value = field.values.firstOrNull().orEmpty(),
            onValueChange = { onFieldChange(field.name, listOf(it)) },
            label = { Text(label) },
            // text-private is always blocked (whole form disabled);
            // masking is defense in depth should that ever regress.
            visualTransformation = if (field.type == ExtensionFieldType.TEXT_PRIVATE) {
                PasswordVisualTransformation()
            } else {
                VisualTransformation.None
            },
            enabled = enabled,
            shape = RoundedCornerShape(Waddle.radius.control),
            modifier = Modifier.fillMaxWidth(),
        )
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun ActionRow(
    actions: List<ExtensionCommandAction>,
    busy: Boolean,
    submitAllowed: Boolean,
    onAction: (ExtensionCommandAction) -> Unit,
) {
    FlowRow(
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs, Alignment.End),
        verticalArrangement = Arrangement.spacedBy(Waddle.space.xs),
        modifier = Modifier
            .fillMaxWidth()
            .padding(top = Waddle.space.xs),
    ) {
        if (ExtensionCommandAction.CANCEL in actions) {
            TextButton(onClick = { onAction(ExtensionCommandAction.CANCEL) }, enabled = !busy) {
                Text(stringResource(R.string.extension_form_cancel), style = Waddle.type.control)
            }
        }
        if (ExtensionCommandAction.PREV in actions) {
            PillButton(
                text = stringResource(R.string.extension_form_back),
                onClick = { onAction(ExtensionCommandAction.PREV) },
                enabled = !busy,
                tone = PillTone.QUIET,
            )
        }
        if (ExtensionCommandAction.NEXT in actions) {
            PillButton(
                text = stringResource(R.string.extension_form_next),
                onClick = { onAction(ExtensionCommandAction.NEXT) },
                enabled = !busy && submitAllowed,
                tone = if (ExtensionCommandAction.COMPLETE in actions) PillTone.QUIET else PillTone.PRIMARY,
            )
        }
        if (ExtensionCommandAction.COMPLETE in actions) {
            PillButton(
                text = stringResource(R.string.extension_form_submit),
                onClick = { onAction(ExtensionCommandAction.COMPLETE) },
                enabled = !busy && submitAllowed,
            )
        }
    }
}

@Composable
private fun fieldLabel(field: ExtensionCommandField): String {
    val base = field.label ?: field.name
    return if (field.required) stringResource(R.string.extension_form_required, base) else base
}

private fun toggleValue(values: List<String>, value: String): List<String> =
    if (value in values) values - value else values + value

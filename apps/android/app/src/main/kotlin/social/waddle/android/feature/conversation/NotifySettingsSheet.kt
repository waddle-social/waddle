package social.waddle.android.feature.conversation

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.RadioButton
import androidx.compose.material3.RadioButtonDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import social.waddle.android.R
import social.waddle.android.theme.Waddle
import social.waddle.client.ffi.WaddleNotifyMode

/** One selectable XEP-0492 mode row (label + hint, web parity copy). */
private data class NotifyModeOption(
    val mode: WaddleNotifyMode,
    val label: Int,
    val hint: Int,
)

private val NOTIFY_MODE_OPTIONS = listOf(
    NotifyModeOption(WaddleNotifyMode.ALWAYS, R.string.notify_mode_always_label, R.string.notify_mode_always_hint),
    NotifyModeOption(
        WaddleNotifyMode.ON_MENTION,
        R.string.notify_mode_on_mention_label,
        R.string.notify_mode_on_mention_hint,
    ),
    NotifyModeOption(WaddleNotifyMode.NEVER, R.string.notify_mode_never_label, R.string.notify_mode_never_hint),
)

/**
 * XEP-0492 per-conversation notification mode picker: a radio list of
 * the three fallback settings, pre-selected from the store's current
 * effective mode. Picking a mode publishes and dismisses; the caller
 * surfaces failures via its snackbar flows.
 */
@Composable
fun NotifySettingsSheet(
    currentMode: WaddleNotifyMode,
    onDismiss: () -> Unit,
    onSelect: (WaddleNotifyMode) -> Unit,
) {
    ConversationSheet(
        onDismissRequest = onDismiss,
        title = stringResource(R.string.notify_settings_title),
        contentPadding = Waddle.space.xs,
    ) {
        Column(modifier = Modifier.selectableGroup()) {
            NOTIFY_MODE_OPTIONS.forEach { option ->
                NotifyModeRow(option, selected = option.mode == currentMode) {
                    onSelect(option.mode)
                    onDismiss()
                }
            }
        }
    }
}

@Composable
private fun NotifyModeRow(option: NotifyModeOption, selected: Boolean, onClick: () -> Unit) {
    val colors = Waddle.colors
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs),
        modifier = Modifier
            .fillMaxWidth()
            .heightIn(min = Waddle.space.xxxl)
            .clip(RoundedCornerShape(Waddle.radius.control))
            .selectable(selected = selected, role = Role.RadioButton, onClick = onClick)
            .padding(vertical = Waddle.space.xs, horizontal = Waddle.space.xxs),
    ) {
        RadioButton(
            selected = selected,
            onClick = null,
            colors = RadioButtonDefaults.colors(selectedColor = colors.action, unselectedColor = colors.fgSoft),
        )
        Column(modifier = Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(Waddle.space.xxs)) {
            Text(text = stringResource(option.label), style = Waddle.type.body, color = colors.fg)
            Text(text = stringResource(option.hint), style = MetaStyle, color = colors.fgMuted)
        }
    }
}

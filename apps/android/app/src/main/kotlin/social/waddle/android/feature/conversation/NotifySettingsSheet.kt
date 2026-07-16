package social.waddle.android.feature.conversation

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import social.waddle.android.R
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
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun NotifySettingsSheet(
    currentMode: WaddleNotifyMode,
    onDismiss: () -> Unit,
    onSelect: (WaddleNotifyMode) -> Unit,
) {
    ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(
            modifier = Modifier
                .navigationBarsPadding()
                .selectableGroup(),
        ) {
            Text(
                text = stringResource(R.string.notify_settings_title),
                style = MaterialTheme.typography.titleMedium,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
            )
            NOTIFY_MODE_OPTIONS.forEach { option ->
                val selected = option.mode == currentMode
                ListItem(
                    headlineContent = { Text(stringResource(option.label)) },
                    supportingContent = { Text(stringResource(option.hint)) },
                    leadingContent = { RadioButton(selected = selected, onClick = null) },
                    colors = ListItemDefaults.colors(containerColor = Color.Transparent),
                    modifier = Modifier.selectable(
                        selected = selected,
                        role = Role.RadioButton,
                        onClick = {
                            onSelect(option.mode)
                            onDismiss()
                        },
                    ),
                )
            }
        }
    }
}

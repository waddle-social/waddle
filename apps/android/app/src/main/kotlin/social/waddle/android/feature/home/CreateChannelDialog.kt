package social.waddle.android.feature.home

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import social.waddle.android.R

/** Semantics tags shared with instrumented tests. */
object CreateChannelDialogTestTags {
    const val NAME_FIELD = "create-channel-name"
    const val DESCRIPTION_FIELD = "create-channel-description"
    const val CREATE_BUTTON = "create-channel-submit"
    const val FORUM_OPTION = "create-channel-type-forum"
}

/**
 * Standalone-channel create dialog (web `CreateChannelDialog` `muc`
 * intent parity): name, description, and the text-vs-forum subtype.
 * Space intents (space / channel-in-space / space-with-channel) are a
 * deferred follow-up — they need the XEP-0060 spaces-node builders.
 */
@Composable
fun CreateChannelDialog(
    onDismiss: () -> Unit,
    onCreate: (name: String, description: String, forum: Boolean) -> Unit,
    creating: Boolean,
) {
    var name by remember { mutableStateOf("") }
    var description by remember { mutableStateOf("") }
    var forum by remember { mutableStateOf(false) }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(text = stringResource(R.string.create_channel_title)) },
        text = {
            Column {
                OutlinedTextField(
                    value = name,
                    onValueChange = { name = it },
                    label = { Text(text = stringResource(R.string.create_channel_name)) },
                    placeholder = { Text(text = stringResource(R.string.create_channel_name_hint)) },
                    singleLine = true,
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(vertical = 4.dp)
                        .testTag(CreateChannelDialogTestTags.NAME_FIELD),
                )
                OutlinedTextField(
                    value = description,
                    onValueChange = { description = it },
                    label = { Text(text = stringResource(R.string.create_channel_description)) },
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(vertical = 4.dp)
                        .testTag(CreateChannelDialogTestTags.DESCRIPTION_FIELD),
                )
                Text(
                    text = stringResource(R.string.create_channel_type),
                    style = MaterialTheme.typography.labelLarge,
                    modifier = Modifier.padding(top = 8.dp, bottom = 4.dp),
                )
                ChannelTypeSelector(forum = forum, onSelect = { forum = it })
            }
        },
        confirmButton = {
            TextButton(
                onClick = { onCreate(name, description, forum) },
                enabled = !creating && name.isNotBlank(),
                modifier = Modifier.testTag(CreateChannelDialogTestTags.CREATE_BUTTON),
            ) {
                Text(text = stringResource(R.string.create_channel_submit))
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) {
                Text(text = stringResource(R.string.action_cancel))
            }
        },
    )
}

@Composable
private fun ChannelTypeSelector(forum: Boolean, onSelect: (Boolean) -> Unit) {
    SingleChoiceSegmentedButtonRow(modifier = Modifier.fillMaxWidth()) {
        SegmentedButton(
            selected = !forum,
            onClick = { onSelect(false) },
            shape = SegmentedButtonDefaults.itemShape(index = 0, count = 2),
        ) {
            Text(text = stringResource(R.string.create_channel_type_text))
        }
        SegmentedButton(
            selected = forum,
            onClick = { onSelect(true) },
            shape = SegmentedButtonDefaults.itemShape(index = 1, count = 2),
            modifier = Modifier.testTag(CreateChannelDialogTestTags.FORUM_OPTION),
        ) {
            Text(text = stringResource(R.string.create_channel_type_forum))
        }
    }
}

package social.waddle.android.feature.room

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import social.waddle.android.R
import social.waddle.android.client.RoomAdminResult
import social.waddle.client.ffi.WaddlePinPermission

/**
 * Owner room-settings sheet (web `EditChannelDialog` parity): name,
 * description, pin-permission policy, and the destructive destroy
 * flow behind a confirm dialog. [onDestroyed] fires after a
 * successful §10.9 destroy so the host can leave the channel screen.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun RoomSettingsSheet(
    viewModel: RoomSettingsViewModel,
    onDismiss: () -> Unit,
    onDestroyed: () -> Unit,
) {
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    var confirmDestroy by remember { mutableStateOf(false) }

    LaunchedEffect(viewModel) {
        viewModel.events.collect { event ->
            when (event) {
                RoomSettingsEvent.Saved -> onDismiss()
                RoomSettingsEvent.Destroyed -> onDestroyed()
            }
        }
    }

    ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(
            modifier = Modifier
                .navigationBarsPadding()
                .padding(horizontal = 16.dp)
                .padding(bottom = 16.dp),
        ) {
            Text(
                text = stringResource(R.string.room_settings_title),
                style = MaterialTheme.typography.titleMedium,
                modifier = Modifier.padding(vertical = 8.dp),
            )
            when {
                state.loading -> CircularProgressIndicator(modifier = Modifier.padding(16.dp))
                state.loadFailed -> Text(
                    text = stringResource(R.string.room_settings_load_failed),
                    color = MaterialTheme.colorScheme.error,
                    modifier = Modifier.padding(vertical = 16.dp),
                )
                else -> RoomSettingsForm(
                    state = state,
                    viewModel = viewModel,
                    onDestroyRequest = { confirmDestroy = true },
                )
            }
        }
    }

    if (confirmDestroy) {
        AlertDialog(
            onDismissRequest = { confirmDestroy = false },
            title = { Text(text = stringResource(R.string.room_destroy_confirm_title)) },
            text = { Text(text = stringResource(R.string.room_destroy_confirm_body)) },
            confirmButton = {
                TextButton(onClick = {
                    confirmDestroy = false
                    viewModel.destroyRoom()
                }) {
                    Text(
                        text = stringResource(R.string.room_destroy_action),
                        color = MaterialTheme.colorScheme.error,
                    )
                }
            },
            dismissButton = {
                TextButton(onClick = { confirmDestroy = false }) {
                    Text(text = stringResource(R.string.action_cancel))
                }
            },
        )
    }
}

@Composable
private fun RoomSettingsForm(
    state: RoomSettingsUiState,
    viewModel: RoomSettingsViewModel,
    onDestroyRequest: () -> Unit,
) {
    OutlinedTextField(
        value = state.name,
        onValueChange = viewModel::setName,
        label = { Text(text = stringResource(R.string.room_settings_name)) },
        singleLine = true,
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 4.dp)
            .testTag(RoomSettingsTestTags.NAME_FIELD),
    )
    OutlinedTextField(
        value = state.description,
        onValueChange = viewModel::setDescription,
        label = { Text(text = stringResource(R.string.room_settings_description)) },
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 4.dp)
            .testTag(RoomSettingsTestTags.DESCRIPTION_FIELD),
    )
    Text(
        text = stringResource(R.string.room_settings_pin_permission),
        style = MaterialTheme.typography.labelLarge,
        modifier = Modifier.padding(top = 12.dp, bottom = 4.dp),
    )
    PinPermissionSelector(selected = state.pinPermission, onSelect = viewModel::setPinPermission)
    // A refused save/destroy must never fail silently — a demoted
    // owner's `forbidden` renders here (the server is the authority).
    state.lastFailure?.let { failure ->
        Text(
            text = stringResource(
                when (failure) {
                    RoomAdminResult.NotConnected -> R.string.action_failed_offline
                    RoomAdminResult.NotPermitted -> R.string.members_action_not_permitted
                    else -> R.string.action_failed
                },
            ),
            color = MaterialTheme.colorScheme.error,
            style = MaterialTheme.typography.bodySmall,
            modifier = Modifier.padding(top = 8.dp),
        )
    }
    Button(
        onClick = viewModel::save,
        enabled = !state.saving && state.name.isNotBlank(),
        modifier = Modifier
            .fillMaxWidth()
            .padding(top = 16.dp)
            .testTag(RoomSettingsTestTags.SAVE_BUTTON),
    ) {
        Text(text = stringResource(R.string.action_save))
    }
    TextButton(
        onClick = onDestroyRequest,
        enabled = !state.saving,
        modifier = Modifier
            .fillMaxWidth()
            .padding(top = 4.dp)
            .testTag(RoomSettingsTestTags.DESTROY_BUTTON),
    ) {
        Text(
            text = stringResource(R.string.room_destroy_action),
            color = MaterialTheme.colorScheme.error,
        )
    }
}

@Composable
private fun PinPermissionSelector(
    selected: WaddlePinPermission?,
    onSelect: (WaddlePinPermission) -> Unit,
) {
    val options = listOf(
        WaddlePinPermission.ADMINS_ONLY to R.string.room_pin_admins_only,
        WaddlePinPermission.ANYONE to R.string.room_pin_anyone,
    )
    SingleChoiceSegmentedButtonRow(modifier = Modifier.fillMaxWidth()) {
        options.forEachIndexed { index, (option, labelRes) ->
            SegmentedButton(
                selected = option == selected,
                onClick = { onSelect(option) },
                shape = SegmentedButtonDefaults.itemShape(index = index, count = options.size),
            ) {
                Text(text = stringResource(labelRes))
            }
        }
    }
}

/** Semantics tags shared with instrumented tests. */
object RoomSettingsTestTags {
    const val NAME_FIELD = "room-settings-name"
    const val DESCRIPTION_FIELD = "room-settings-description"
    const val SAVE_BUTTON = "room-settings-save"
    const val DESTROY_BUTTON = "room-settings-destroy"
}

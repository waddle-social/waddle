package social.waddle.android.feature.room

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExperimentalMaterial3ExpressiveApi
import androidx.compose.material3.LoadingIndicator
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import social.waddle.android.R
import social.waddle.android.client.RoomAdminResult
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.ChoiceOption
import social.waddle.android.ui.HuddleFieldShape
import social.waddle.android.ui.KickerText
import social.waddle.android.ui.SegmentedChoice
import social.waddle.android.ui.SheetTitle
import social.waddle.android.ui.huddle.PillButton
import social.waddle.android.ui.huddleFieldColors
import social.waddle.client.ffi.WaddlePinPermission

/**
 * Owner room-settings sheet (web `EditChannelDialog` parity): name,
 * description, pin-permission policy, and the destructive destroy
 * flow behind a confirm dialog. [onDestroyed] fires after a
 * successful §10.9 destroy so the host can leave the channel screen.
 */
@OptIn(ExperimentalMaterial3Api::class, ExperimentalMaterial3ExpressiveApi::class)
@Composable
fun RoomSettingsSheet(
    viewModel: RoomSettingsViewModel,
    onDismiss: () -> Unit,
    onDestroyed: () -> Unit,
) {
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    var confirmDestroy by remember { mutableStateOf(false) }
    val colors = Waddle.colors

    LaunchedEffect(viewModel) {
        viewModel.events.collect { event ->
            when (event) {
                RoomSettingsEvent.Saved -> onDismiss()
                RoomSettingsEvent.Destroyed -> onDestroyed()
            }
        }
    }

    ModalBottomSheet(onDismissRequest = onDismiss, containerColor = colors.surface) {
        Column(
            modifier = Modifier
                .navigationBarsPadding()
                .imePadding()
                .verticalScroll(rememberScrollState())
                .padding(bottom = Waddle.space.md),
        ) {
            SheetTitle(text = stringResource(R.string.room_settings_title))
            when {
                state.loading -> Box(
                    contentAlignment = Alignment.Center,
                    modifier = Modifier
                        .fillMaxWidth()
                        .heightIn(min = LOADING_HEIGHT),
                ) {
                    LoadingIndicator(color = colors.action, modifier = Modifier.size(Waddle.space.xxl))
                }
                state.loadFailed -> Text(
                    text = stringResource(R.string.room_settings_load_failed),
                    style = Waddle.type.body,
                    color = colors.dangerText,
                    modifier = Modifier.padding(horizontal = Waddle.space.md, vertical = Waddle.space.md),
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
            containerColor = colors.surface,
            title = { Text(text = stringResource(R.string.room_destroy_confirm_title)) },
            text = { Text(text = stringResource(R.string.room_destroy_confirm_body), color = colors.fgMuted) },
            confirmButton = {
                TextButton(onClick = {
                    confirmDestroy = false
                    viewModel.destroyRoom()
                }) {
                    Text(text = stringResource(R.string.room_destroy_action), color = colors.dangerText)
                }
            },
            dismissButton = {
                TextButton(onClick = { confirmDestroy = false }) {
                    Text(text = stringResource(R.string.action_cancel), color = colors.fg)
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
    val colors = Waddle.colors
    Column(
        verticalArrangement = Arrangement.spacedBy(Waddle.space.sm),
        modifier = Modifier.padding(horizontal = Waddle.space.md),
    ) {
        OutlinedTextField(
            value = state.name,
            onValueChange = viewModel::setName,
            label = { Text(text = stringResource(R.string.room_settings_name)) },
            singleLine = true,
            shape = HuddleFieldShape,
            colors = huddleFieldColors(),
            modifier = Modifier
                .fillMaxWidth()
                .testTag(RoomSettingsTestTags.NAME_FIELD),
        )
        OutlinedTextField(
            value = state.description,
            onValueChange = viewModel::setDescription,
            label = { Text(text = stringResource(R.string.room_settings_description)) },
            minLines = 2,
            shape = HuddleFieldShape,
            colors = huddleFieldColors(),
            modifier = Modifier
                .fillMaxWidth()
                .testTag(RoomSettingsTestTags.DESCRIPTION_FIELD),
        )
        KickerText(
            text = stringResource(R.string.room_settings_pin_permission),
            modifier = Modifier.padding(top = Waddle.space.xs),
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
                color = colors.dangerText,
                style = Waddle.type.control,
            )
        }
        PillButton(
            text = stringResource(R.string.action_save),
            onClick = viewModel::save,
            enabled = !state.saving && state.name.isNotBlank(),
            modifier = Modifier
                .fillMaxWidth()
                .padding(top = Waddle.space.xs)
                .heightIn(min = Waddle.control.touch)
                .testTag(RoomSettingsTestTags.SAVE_BUTTON),
        )
        TextButton(
            onClick = onDestroyRequest,
            enabled = !state.saving,
            modifier = Modifier
                .fillMaxWidth()
                .testTag(RoomSettingsTestTags.DESTROY_BUTTON),
        ) {
            Text(text = stringResource(R.string.room_destroy_action), color = colors.dangerText)
        }
    }
}

@Composable
private fun PinPermissionSelector(
    selected: WaddlePinPermission?,
    onSelect: (WaddlePinPermission) -> Unit,
) {
    SegmentedChoice(
        options = listOf(
            ChoiceOption(WaddlePinPermission.ADMINS_ONLY, stringResource(R.string.room_pin_admins_only)),
            ChoiceOption(WaddlePinPermission.ANYONE, stringResource(R.string.room_pin_anyone)),
        ),
        // Nothing selected until the room's configuration loads.
        selected = selected,
        onSelect = { option -> option?.let(onSelect) },
    )
}

private val LOADING_HEIGHT = 120.dp

/** Semantics tags shared with instrumented tests. */
object RoomSettingsTestTags {
    const val NAME_FIELD = "room-settings-name"
    const val DESCRIPTION_FIELD = "room-settings-description"
    const val SAVE_BUTTON = "room-settings-save"
    const val DESTROY_BUTTON = "room-settings-destroy"
}

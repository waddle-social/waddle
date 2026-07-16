package social.waddle.android.feature.settings

import android.content.Context
import android.content.Intent
import android.os.PowerManager
import android.provider.Settings
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.defaultMinSize
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.outlined.AccountCircle
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.core.net.toUri
import androidx.lifecycle.compose.LifecycleResumeEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import coil3.compose.AsyncImage
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.client.prefs.ThemeMode

/**
 * Account, theme mode, notification toggles, battery-optimization
 * exemption, logout.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsScreen(onBack: () -> Unit) {
    val graph = LocalAppGraph.current
    val viewModel: SettingsViewModel = viewModel(factory = SettingsViewModel.factory(graph))
    val state by viewModel.uiState.collectAsStateWithLifecycle()

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(text = stringResource(R.string.settings_title)) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(
                            Icons.AutoMirrored.Filled.ArrowBack,
                            contentDescription = stringResource(R.string.action_back),
                        )
                    }
                },
            )
        },
    ) { padding ->
        Column(
            modifier = Modifier
                .padding(padding)
                .fillMaxSize()
                .verticalScroll(rememberScrollState()),
        ) {
            AccountRow(state = state)
            SectionHeader(text = stringResource(R.string.settings_appearance))
            ThemeSelector(selected = state.theme, onSelect = viewModel::setTheme)
            SectionHeader(text = stringResource(R.string.settings_notifications))
            ToggleRow(
                title = stringResource(R.string.settings_notifications_enabled),
                checked = state.notificationsEnabled,
                onCheckedChange = viewModel::setNotificationsEnabled,
                toggleTestTag = SettingsScreenTestTags.NOTIFICATIONS_TOGGLE,
            )
            ToggleRow(
                title = stringResource(R.string.settings_message_sounds),
                checked = state.messageSoundsEnabled,
                onCheckedChange = viewModel::setMessageSoundsEnabled,
                toggleTestTag = SettingsScreenTestTags.MESSAGE_SOUNDS_TOGGLE,
            )
            SectionHeader(text = stringResource(R.string.settings_privacy))
            ToggleRow(
                title = stringResource(R.string.settings_read_receipts),
                checked = state.readReceiptsEnabled,
                onCheckedChange = viewModel::setReadReceiptsEnabled,
                toggleTestTag = SettingsScreenTestTags.READ_RECEIPTS_TOGGLE,
            )
            SectionHeader(text = stringResource(R.string.settings_battery))
            BatteryOptimizationRow()
            Button(
                onClick = viewModel::logout,
                colors = ButtonDefaults.buttonColors(
                    containerColor = MaterialTheme.colorScheme.error,
                    contentColor = MaterialTheme.colorScheme.onError,
                ),
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(16.dp)
                    .defaultMinSize(minHeight = 48.dp),
            ) {
                Text(text = stringResource(R.string.action_logout))
            }
        }
    }
}

@Composable
private fun AccountRow(state: SettingsUiState) {
    ListItem(
        headlineContent = {
            Text(text = state.username ?: stringResource(R.string.settings_account))
        },
        supportingContent = { state.jid?.let { Text(text = it) } },
        leadingContent = {
            if (state.avatarUrl != null) {
                AsyncImage(
                    model = state.avatarUrl,
                    contentDescription = stringResource(R.string.settings_avatar),
                    contentScale = ContentScale.Crop,
                    modifier = Modifier
                        .size(40.dp)
                        .clip(CircleShape),
                )
            } else {
                Icon(
                    Icons.Outlined.AccountCircle,
                    contentDescription = stringResource(R.string.settings_avatar),
                    modifier = Modifier.size(40.dp),
                )
            }
        },
    )
}

@Composable
private fun ThemeSelector(selected: ThemeMode, onSelect: (ThemeMode) -> Unit) {
    val modes = listOf(ThemeMode.LIGHT, ThemeMode.SYSTEM, ThemeMode.DARK)
    SingleChoiceSegmentedButtonRow(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 8.dp),
    ) {
        modes.forEachIndexed { index, mode ->
            SegmentedButton(
                selected = mode == selected,
                onClick = { onSelect(mode) },
                shape = SegmentedButtonDefaults.itemShape(index = index, count = modes.size),
            ) {
                Text(text = stringResource(mode.labelRes()))
            }
        }
    }
}

@Composable
private fun ToggleRow(
    title: String,
    checked: Boolean,
    onCheckedChange: (Boolean) -> Unit,
    toggleTestTag: String,
) {
    ListItem(
        headlineContent = { Text(text = title) },
        trailingContent = {
            Switch(
                checked = checked,
                onCheckedChange = onCheckedChange,
                modifier = Modifier.testTag(toggleTestTag),
            )
        },
    )
}

/** Semantics tags shared with instrumented tests. */
object SettingsScreenTestTags {
    const val NOTIFICATIONS_TOGGLE = "settings-notifications-toggle"
    const val MESSAGE_SOUNDS_TOGGLE = "settings-message-sounds-toggle"
    const val READ_RECEIPTS_TOGGLE = "settings-read-receipts-toggle"
}

@Composable
private fun BatteryOptimizationRow() {
    val context = LocalContext.current
    var exempt by remember { mutableStateOf(isIgnoringBatteryOptimizations(context)) }

    // The system dialog returns without a result; re-check on resume.
    LifecycleResumeEffect(Unit) {
        exempt = isIgnoringBatteryOptimizations(context)
        onPauseOrDispose {}
    }

    ListItem(
        headlineContent = { Text(text = stringResource(R.string.settings_battery_row_title)) },
        supportingContent = {
            Text(
                text = stringResource(
                    if (exempt) {
                        R.string.settings_battery_exempt
                    } else {
                        R.string.settings_battery_not_exempt
                    },
                ),
            )
        },
        trailingContent = {
            if (!exempt) {
                TextButton(onClick = { requestBatteryExemption(context) }) {
                    Text(text = stringResource(R.string.settings_battery_request))
                }
            }
        },
    )
}

private fun isIgnoringBatteryOptimizations(context: Context): Boolean =
    context.getSystemService(PowerManager::class.java)
        ?.isIgnoringBatteryOptimizations(context.packageName) == true

private fun requestBatteryExemption(context: Context) {
    val intent = Intent(
        Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS,
        "package:${context.packageName}".toUri(),
    )
    runCatching { context.startActivity(intent) }
}

@Composable
private fun SectionHeader(text: String) {
    Text(
        text = text,
        style = MaterialTheme.typography.titleSmall,
        color = MaterialTheme.colorScheme.primary,
        modifier = Modifier.padding(start = 16.dp, top = 16.dp, end = 16.dp, bottom = 4.dp),
    )
}

private fun ThemeMode.labelRes(): Int = when (this) {
    ThemeMode.LIGHT -> R.string.settings_theme_light
    ThemeMode.SYSTEM -> R.string.settings_theme_system
    ThemeMode.DARK -> R.string.settings_theme_dark
}

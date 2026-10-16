package social.waddle.android.feature.settings

import android.app.Activity
import android.content.Context
import android.content.ContextWrapper
import android.content.Intent
import android.graphics.BitmapFactory
import android.os.PowerManager
import android.provider.Settings
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.Image
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.defaultMinSize
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.outlined.KeyboardArrowRight
import androidx.compose.material.icons.automirrored.outlined.Logout
import androidx.compose.material.icons.outlined.Group
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.core.net.toUri
import androidx.lifecycle.compose.LifecycleResumeEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import coil3.compose.AsyncImage
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.avatar.PeerAvatar
import social.waddle.android.client.prefs.ThemeMode
import social.waddle.android.client.push.PushStatus
import social.waddle.android.theme.MonoMeta
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.GroupDivider
import social.waddle.android.ui.GroupedSection
import social.waddle.android.ui.WaddleTopBar
import social.waddle.android.ui.groupedRowColors
import social.waddle.client.ffi.WaddleAvatar

/**
 * Account, theme mode, notification toggles, battery-optimization
 * exemption, logout.
 */
@Composable
fun SettingsScreen(
    onBack: () -> Unit,
    onOpenCommunityUsers: () -> Unit = {},
    onOpenProfile: () -> Unit = {},
) {
    val graph = LocalAppGraph.current
    val viewModel: SettingsViewModel = viewModel(factory = SettingsViewModel.factory(graph))
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    val isCommunityOwner by viewModel.isCommunityOwner.collectAsStateWithLifecycle()
    val selfAvatar by viewModel.selfAvatar.collectAsStateWithLifecycle()
    val delivery by viewModel.deliveryState.collectAsStateWithLifecycle()
    val context = LocalContext.current
    LifecycleResumeEffect(Unit) {
        // A distributor may have been installed or chosen meanwhile.
        graph.pushDistributor.refresh()
        onPauseOrDispose {}
    }
    SettingsContent(
        state = state,
        selfAvatar = selfAvatar,
        isCommunityOwner = isCommunityOwner,
        delivery = delivery,
        actions = SettingsActions(
            onBack = onBack,
            onOpenProfile = onOpenProfile,
            onOpenCommunityUsers = onOpenCommunityUsers,
            onSetTheme = viewModel::setTheme,
            onSetNotificationsEnabled = viewModel::setNotificationsEnabled,
            onSetMessageSoundsEnabled = viewModel::setMessageSoundsEnabled,
            onSetReadReceiptsEnabled = viewModel::setReadReceiptsEnabled,
            onLogout = viewModel::logout,
            onSetStayConnected = viewModel::setStayConnected,
            onPickDistributor = { context.findActivity()?.let(graph.pushDistributor::pick) },
        ),
    )
}

private tailrec fun Context.findActivity(): Activity? = when (this) {
    is Activity -> this
    is ContextWrapper -> baseContext.findActivity()
    else -> null
}

/** What the settings surface can ask for. */
internal data class SettingsActions(
    val onBack: () -> Unit,
    val onOpenProfile: () -> Unit,
    val onOpenCommunityUsers: () -> Unit,
    val onSetTheme: (ThemeMode) -> Unit,
    val onSetNotificationsEnabled: (Boolean) -> Unit,
    val onSetMessageSoundsEnabled: (Boolean) -> Unit,
    val onSetReadReceiptsEnabled: (Boolean) -> Unit,
    val onLogout: () -> Unit,
    val onSetStayConnected: (Boolean) -> Unit = {},
    val onPickDistributor: () -> Unit = {},
)

/** Stateless settings surface: grouped cards on the screen ground. */
@Composable
internal fun SettingsContent(
    state: SettingsUiState,
    selfAvatar: WaddleAvatar?,
    isCommunityOwner: Boolean,
    actions: SettingsActions,
    delivery: DeliveryUiState = DeliveryUiState(),
) {
    Scaffold(
        topBar = {
            WaddleTopBar(
                title = { Text(text = stringResource(R.string.settings_title)) },
                navigationIcon = {
                    IconButton(onClick = actions.onBack) {
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
                .verticalScroll(rememberScrollState())
                .padding(bottom = Waddle.space.xl),
        ) {
            GroupedSection(title = null, modifier = Modifier.padding(top = Waddle.space.xs)) {
                AccountRow(state = state, selfAvatar = selfAvatar, onOpenProfile = actions.onOpenProfile)
            }
            GroupedSection(title = stringResource(R.string.settings_appearance)) {
                ThemeSelector(selected = state.theme, onSelect = actions.onSetTheme)
            }
            GroupedSection(title = stringResource(R.string.settings_notifications)) {
                ToggleRow(
                    title = stringResource(R.string.settings_notifications_enabled),
                    checked = state.notificationsEnabled,
                    onCheckedChange = actions.onSetNotificationsEnabled,
                    toggleTestTag = SettingsScreenTestTags.NOTIFICATIONS_TOGGLE,
                )
                GroupDivider()
                ToggleRow(
                    title = stringResource(R.string.settings_message_sounds),
                    checked = state.messageSoundsEnabled,
                    onCheckedChange = actions.onSetMessageSoundsEnabled,
                    toggleTestTag = SettingsScreenTestTags.MESSAGE_SOUNDS_TOGGLE,
                )
            }
            GroupedSection(title = stringResource(R.string.settings_privacy)) {
                ToggleRow(
                    title = stringResource(R.string.settings_read_receipts),
                    checked = state.readReceiptsEnabled,
                    onCheckedChange = actions.onSetReadReceiptsEnabled,
                    toggleTestTag = SettingsScreenTestTags.READ_RECEIPTS_TOGGLE,
                )
            }
            // Reduced-scope community admin (web admin console has 6
            // desktop operator panels; mobile ships the V1 users list
            // only). Entry hidden unless the owner probe succeeds; the
            // server re-authorizes every admin command regardless.
            if (isCommunityOwner) {
                GroupedSection(title = stringResource(R.string.settings_community_admin)) {
                    ListItem(
                        headlineContent = { Text(text = stringResource(R.string.community_users_title)) },
                        supportingContent = { Text(text = stringResource(R.string.community_users_subtitle)) },
                        leadingContent = { Icon(Icons.Outlined.Group, contentDescription = null) },
                        trailingContent = { Chevron() },
                        colors = groupedRowColors(),
                        modifier = Modifier
                            .clickable(onClick = actions.onOpenCommunityUsers)
                            .testTag(SettingsScreenTestTags.COMMUNITY_USERS_ROW),
                    )
                }
            }
            GroupedSection(title = stringResource(R.string.settings_delivery)) {
                DeliverySection(delivery = delivery, actions = actions)
            }
            OutlinedButton(
                onClick = actions.onLogout,
                shape = RoundedCornerShape(Waddle.radius.lg),
                border = BorderStroke(1.dp, Waddle.colors.hairline),
                colors = ButtonDefaults.outlinedButtonColors(contentColor = Waddle.colors.dangerText),
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = Waddle.space.md, vertical = Waddle.space.lg)
                    .defaultMinSize(minHeight = 48.dp),
            ) {
                Icon(
                    Icons.AutoMirrored.Outlined.Logout,
                    contentDescription = null,
                    modifier = Modifier
                        .padding(end = Waddle.space.xs)
                        .size(18.dp),
                )
                Text(text = stringResource(R.string.action_logout))
            }
        }
    }
}

/** Push status, the distributor, the stay-connected switch and battery. */
@Composable
private fun DeliverySection(delivery: DeliveryUiState, actions: SettingsActions) {
    Text(
        text = deliveryStatusText(delivery),
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier
            .padding(horizontal = Waddle.space.md, vertical = Waddle.space.sm)
            .testTag(SettingsScreenTestTags.DELIVERY_STATUS),
    )
    if (delivery.distributorInstalled) {
        GroupDivider()
        ListItem(
            headlineContent = { Text(text = stringResource(R.string.settings_delivery_distributor)) },
            supportingContent = {
                Text(text = delivery.distributorLabel ?: stringResource(R.string.settings_delivery_distributor_none))
            },
            trailingContent = { Chevron() },
            colors = groupedRowColors(),
            modifier = Modifier.clickable(onClick = actions.onPickDistributor),
        )
    }
    GroupDivider()
    ListItem(
        headlineContent = { Text(text = stringResource(R.string.settings_stay_connected)) },
        supportingContent = { Text(text = stringResource(R.string.settings_stay_connected_body)) },
        trailingContent = {
            Switch(
                checked = delivery.stayConnected,
                onCheckedChange = actions.onSetStayConnected,
                modifier = Modifier.testTag(SettingsScreenTestTags.STAY_CONNECTED_TOGGLE),
            )
        },
        colors = groupedRowColors(),
    )
    // Battery exemption only matters while a connection is held.
    if (delivery.stayConnected) {
        GroupDivider()
        BatteryOptimizationRow()
    }
}

@Composable
private fun deliveryStatusText(delivery: DeliveryUiState): String {
    val distributor = delivery.distributorLabel
    return when (delivery.status) {
        PushStatus.ACTIVE -> stringResource(R.string.settings_delivery_push_active, distributor.orEmpty())
        PushStatus.PENDING -> stringResource(R.string.settings_delivery_push_pending, distributor.orEmpty())
        PushStatus.UNSUPPORTED -> stringResource(R.string.settings_delivery_unsupported)
        PushStatus.FAILED -> stringResource(R.string.settings_delivery_failed)
        PushStatus.NONE -> if (delivery.distributorInstalled) {
            stringResource(R.string.settings_delivery_choose_distributor)
        } else {
            stringResource(R.string.settings_delivery_no_distributor)
        }
    }
}

@Composable
private fun Chevron() {
    Icon(
        Icons.AutoMirrored.Outlined.KeyboardArrowRight,
        contentDescription = null,
        tint = MaterialTheme.colorScheme.onSurfaceVariant,
    )
}

@Composable
private fun AccountRow(
    state: SettingsUiState,
    selfAvatar: WaddleAvatar?,
    onOpenProfile: () -> Unit,
) {
    // XMPP-published XEP-0084 bytes win over the REST avatar URL.
    val xmppAvatar = remember(selfAvatar?.id) {
        selfAvatar?.data?.let { bytes ->
            BitmapFactory.decodeByteArray(bytes, 0, bytes.size)?.asImageBitmap()
        }
    }
    val avatarLabel = stringResource(R.string.settings_avatar)
    ListItem(
        headlineContent = {
            Text(
                text = state.username ?: stringResource(R.string.settings_account),
                style = MaterialTheme.typography.titleMedium,
            )
        },
        supportingContent = { state.jid?.let { Text(text = it, style = MonoMeta) } },
        leadingContent = {
            when {
                xmppAvatar != null -> Image(
                    bitmap = xmppAvatar,
                    contentDescription = avatarLabel,
                    contentScale = ContentScale.Crop,
                    modifier = Modifier
                        .size(ACCOUNT_AVATAR_SIZE)
                        .clip(CircleShape),
                )
                state.avatarUrl != null -> AsyncImage(
                    model = state.avatarUrl,
                    contentDescription = avatarLabel,
                    contentScale = ContentScale.Crop,
                    modifier = Modifier
                        .size(ACCOUNT_AVATAR_SIZE)
                        .clip(CircleShape),
                )
                // No image yet: initials, like every other person surface.
                else -> PeerAvatar(
                    jid = null,
                    displayName = state.username ?: state.jid?.substringBefore('@').orEmpty(),
                    size = ACCOUNT_AVATAR_SIZE,
                    modifier = Modifier.semantics { contentDescription = avatarLabel },
                )
            }
        },
        trailingContent = { Chevron() },
        colors = groupedRowColors(),
        modifier = Modifier
            .clickable(onClick = onOpenProfile)
            .testTag(SettingsScreenTestTags.ACCOUNT_ROW),
    )
}

private val ACCOUNT_AVATAR_SIZE = 52.dp

@Composable
private fun ThemeSelector(selected: ThemeMode, onSelect: (ThemeMode) -> Unit) {
    val modes = listOf(ThemeMode.LIGHT, ThemeMode.SYSTEM, ThemeMode.DARK)
    SingleChoiceSegmentedButtonRow(
        modifier = Modifier
            .fillMaxWidth()
            .padding(Waddle.space.sm),
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
        colors = groupedRowColors(),
    )
}

/** Semantics tags shared with instrumented tests. */
object SettingsScreenTestTags {
    const val ACCOUNT_ROW = "settings-account-row"
    const val NOTIFICATIONS_TOGGLE = "settings-notifications-toggle"
    const val MESSAGE_SOUNDS_TOGGLE = "settings-message-sounds-toggle"
    const val READ_RECEIPTS_TOGGLE = "settings-read-receipts-toggle"
    const val COMMUNITY_USERS_ROW = "settings-community-users-row"
    const val DELIVERY_STATUS = "settings-delivery-status"
    const val STAY_CONNECTED_TOGGLE = "settings-stay-connected-toggle"
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
        colors = groupedRowColors(),
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

private fun ThemeMode.labelRes(): Int = when (this) {
    ThemeMode.LIGHT -> R.string.settings_theme_light
    ThemeMode.SYSTEM -> R.string.settings_theme_system
    ThemeMode.DARK -> R.string.settings_theme_dark
}

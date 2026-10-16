package social.waddle.android.feature.settings

import android.app.Activity
import android.content.Context
import android.content.ContextWrapper
import android.content.Intent
import android.graphics.BitmapFactory
import android.os.PowerManager
import android.provider.Settings
import androidx.compose.foundation.Image
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.automirrored.outlined.Logout
import androidx.compose.material.icons.outlined.DarkMode
import androidx.compose.material.icons.outlined.ErrorOutline
import androidx.compose.material.icons.outlined.Group
import androidx.compose.material.icons.outlined.LightMode
import androidx.compose.material.icons.outlined.NotificationsActive
import androidx.compose.material.icons.outlined.Sync
import androidx.compose.material.icons.outlined.Wifi
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.core.net.toUri
import androidx.lifecycle.compose.LifecycleResumeEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import coil3.compose.AsyncImage
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.avatar.PeerAvatar
import social.waddle.android.client.prefs.PushEndpointKind
import social.waddle.android.client.prefs.ThemeMode
import social.waddle.android.client.push.PushStatus
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.ChoiceOption
import social.waddle.android.ui.GlyphTile
import social.waddle.android.ui.GroupDivider
import social.waddle.android.ui.GroupedRow
import social.waddle.android.ui.GroupedSection
import social.waddle.android.ui.KickerText
import social.waddle.android.ui.RowChevron
import social.waddle.android.ui.SegmentedChoice
import social.waddle.android.ui.WaddleTopBar
import social.waddle.android.ui.dangerSubtle
import social.waddle.android.ui.huddle.AvatarSize
import social.waddle.android.ui.huddleSwitchColors
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
            onSetWallpaperColors = viewModel::setWallpaperColors,
            onSetNotificationsEnabled = viewModel::setNotificationsEnabled,
            onSetMessageSoundsEnabled = viewModel::setMessageSoundsEnabled,
            onSetReadReceiptsEnabled = viewModel::setReadReceiptsEnabled,
            onLogout = viewModel::logout,
            onSetStayConnected = viewModel::setStayConnected,
            onPickDistributor = { context.findActivity()?.let(graph.push::useUnifiedPush) },
            onUseFcm = graph.push::useFcm,
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
    val onUseFcm: () -> Unit = {},
    val onSetWallpaperColors: (Boolean) -> Unit = {},
)

/** Stateless settings surface: Huddle card groups on the screen ground. */
@Composable
internal fun SettingsContent(
    state: SettingsUiState,
    selfAvatar: WaddleAvatar?,
    isCommunityOwner: Boolean,
    actions: SettingsActions,
    delivery: DeliveryUiState = DeliveryUiState(),
) {
    Scaffold(
        containerColor = Waddle.colors.bg,
        topBar = {
            WaddleTopBar(
                title = { Text(text = stringResource(R.string.settings_title)) },
                navigationIcon = {
                    IconButton(onClick = actions.onBack) {
                        Icon(
                            Icons.AutoMirrored.Outlined.ArrowBack,
                            contentDescription = stringResource(R.string.action_back),
                        )
                    }
                },
            )
        },
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .consumeWindowInsets(padding)
                .verticalScroll(rememberScrollState())
                .padding(padding)
                .padding(bottom = Waddle.space.xl),
        ) {
            GroupedSection(title = null, modifier = Modifier.padding(top = Waddle.space.xs)) {
                AccountRow(state = state, selfAvatar = selfAvatar, onOpenProfile = actions.onOpenProfile)
            }
            GroupedSection(title = stringResource(R.string.settings_appearance)) {
                ThemeSelector(selected = state.theme, onSelect = actions.onSetTheme)
                GroupDivider()
                ToggleRow(
                    title = stringResource(R.string.settings_wallpaper_colors),
                    supporting = stringResource(R.string.settings_wallpaper_colors_detail),
                    checked = state.wallpaperColors,
                    onCheckedChange = actions.onSetWallpaperColors,
                    toggleTestTag = SettingsScreenTestTags.WALLPAPER_COLORS_TOGGLE,
                )
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
            GroupedSection(title = stringResource(R.string.settings_delivery)) {
                DeliverySection(delivery = delivery, actions = actions)
            }
            // Reduced-scope community admin (web admin console has 6
            // desktop operator panels; mobile ships the V1 users list
            // only). Entry hidden unless the owner probe succeeds; the
            // server re-authorizes every admin command regardless.
            if (isCommunityOwner) {
                GroupedSection(title = stringResource(R.string.settings_community_admin)) {
                    GroupedRow(
                        title = stringResource(R.string.community_users_title),
                        supporting = stringResource(R.string.community_users_subtitle),
                        leading = { GlyphTile(Icons.Outlined.Group) },
                        trailing = { RowChevron() },
                        onClick = actions.onOpenCommunityUsers,
                        modifier = Modifier.testTag(SettingsScreenTestTags.COMMUNITY_USERS_ROW),
                    )
                }
            }
            GroupedSection(title = null, modifier = Modifier.padding(top = Waddle.space.lg)) {
                GroupedRow(
                    title = stringResource(R.string.action_logout),
                    titleColor = Waddle.colors.dangerText,
                    leading = {
                        GlyphTile(
                            icon = Icons.AutoMirrored.Outlined.Logout,
                            tint = Waddle.colors.dangerText,
                            container = Waddle.colors.dangerSubtle(),
                        )
                    },
                    onClick = actions.onLogout,
                )
            }
        }
    }
}

/** What background delivery is doing, first; then the controls that change it. */
@Composable
private fun DeliverySection(delivery: DeliveryUiState, actions: SettingsActions) {
    DeliveryStatusRow(delivery)
    if (delivery.fcmAvailable) {
        GroupDivider()
        val usingFcm = delivery.endpointKind == PushEndpointKind.FCM
        GroupedRow(
            title = stringResource(R.string.settings_delivery_fcm),
            supporting = stringResource(
                if (usingFcm) R.string.settings_delivery_fcm_in_use else R.string.settings_delivery_fcm_use,
            ),
            trailing = { RowChevron() },
            // Always available: it also brings back FCM after a distributor
            // choice that never produced an endpoint.
            onClick = actions.onUseFcm,
        )
    }
    if (delivery.distributorInstalled) {
        GroupDivider()
        GroupedRow(
            title = stringResource(R.string.settings_delivery_distributor),
            supporting = delivery.distributorLabel
                ?.takeIf { delivery.endpointKind != PushEndpointKind.FCM }
                ?: stringResource(R.string.settings_delivery_distributor_none),
            trailing = { RowChevron() },
            onClick = actions.onPickDistributor,
        )
    }
    // The Play build holds no background connection: push is the only
    // background delivery there.
    if (!delivery.stayConnectedSupported) return
    GroupDivider()
    ToggleRow(
        title = stringResource(R.string.settings_stay_connected),
        supporting = stringResource(R.string.settings_stay_connected_body),
        checked = delivery.stayConnected,
        onCheckedChange = actions.onSetStayConnected,
        toggleTestTag = SettingsScreenTestTags.STAY_CONNECTED_TOGGLE,
    )
    // Battery exemption only matters while a connection is held.
    if (delivery.stayConnected) {
        GroupDivider()
        BatteryOptimizationRow()
    }
}

/** How a delivery status reads: icon, words, tag and tone. */
private data class DeliveryLook(
    val icon: ImageVector,
    val title: String,
    val detail: String,
    val tag: String,
    val tone: DeliveryTone,
)

private enum class DeliveryTone { GOOD, WAITING, NEUTRAL, PROBLEM }

@Composable
private fun deliveryLookOf(delivery: DeliveryUiState): DeliveryLook {
    val distributor = if (delivery.endpointKind == PushEndpointKind.FCM) {
        stringResource(R.string.settings_delivery_fcm_name)
    } else {
        delivery.distributorLabel.orEmpty()
    }
    val off = stringResource(R.string.huddle_delivery_tag_off)
    if (!delivery.stayConnectedSupported) noBackgroundLookOf(delivery, off)?.let { return it }
    val connectedTitle = stringResource(R.string.huddle_delivery_connected_title)
    return when (delivery.status) {
        PushStatus.ACTIVE -> DeliveryLook(
            icon = Icons.Outlined.NotificationsActive,
            title = stringResource(R.string.huddle_delivery_push_title, distributor),
            detail = stringResource(R.string.huddle_delivery_push_detail),
            tag = stringResource(R.string.huddle_delivery_tag_on),
            tone = DeliveryTone.GOOD,
        )
        PushStatus.PENDING -> DeliveryLook(
            icon = Icons.Outlined.Sync,
            title = stringResource(R.string.huddle_delivery_pending_title),
            detail = stringResource(R.string.huddle_delivery_pending_detail, distributor),
            tag = stringResource(R.string.huddle_delivery_tag_pending),
            tone = DeliveryTone.WAITING,
        )
        PushStatus.UNSUPPORTED -> DeliveryLook(
            icon = Icons.Outlined.Wifi,
            title = connectedTitle,
            detail = stringResource(R.string.huddle_delivery_unsupported_detail),
            tag = off,
            tone = DeliveryTone.NEUTRAL,
        )
        PushStatus.FAILED -> DeliveryLook(
            icon = Icons.Outlined.ErrorOutline,
            title = stringResource(R.string.huddle_delivery_failed_title),
            detail = stringResource(R.string.huddle_delivery_failed_detail),
            tag = stringResource(R.string.huddle_delivery_tag_failed),
            tone = DeliveryTone.PROBLEM,
        )
        PushStatus.NONE -> DeliveryLook(
            icon = Icons.Outlined.Wifi,
            title = if (delivery.distributorInstalled) {
                stringResource(R.string.huddle_delivery_off_title)
            } else {
                connectedTitle
            },
            detail = if (delivery.distributorInstalled) {
                stringResource(R.string.huddle_delivery_choose_detail)
            } else {
                stringResource(R.string.huddle_delivery_no_distributor_detail)
            },
            tag = off,
            tone = DeliveryTone.NEUTRAL,
        )
    }
}

/**
 * Without a background connection to fall back on (the Play build), any
 * status but working push means messages arrive only while Waddle is
 * open; say so plainly. `null` for the statuses that read the same.
 */
@Composable
private fun noBackgroundLookOf(delivery: DeliveryUiState, off: String): DeliveryLook? = when (delivery.status) {
    PushStatus.ACTIVE, PushStatus.PENDING -> null
    PushStatus.FAILED -> DeliveryLook(
        icon = Icons.Outlined.ErrorOutline,
        title = stringResource(R.string.huddle_delivery_failed_title),
        detail = stringResource(R.string.huddle_delivery_failed_no_background_detail),
        tag = stringResource(R.string.huddle_delivery_tag_failed),
        tone = DeliveryTone.PROBLEM,
    )
    PushStatus.UNSUPPORTED, PushStatus.NONE -> DeliveryLook(
        icon = Icons.Outlined.ErrorOutline,
        title = stringResource(R.string.huddle_delivery_no_background_title),
        detail = stringResource(R.string.huddle_delivery_no_background_detail),
        tag = off,
        tone = DeliveryTone.PROBLEM,
    )
}

@Composable
private fun DeliveryStatusRow(delivery: DeliveryUiState) {
    val colors = Waddle.colors
    val look = deliveryLookOf(delivery)
    val (ink, wash) = when (look.tone) {
        DeliveryTone.GOOD -> colors.actionText to colors.actionSubtle
        DeliveryTone.WAITING -> colors.warningText to colors.surface2
        DeliveryTone.NEUTRAL -> colors.fgMuted to colors.surface2
        DeliveryTone.PROBLEM -> colors.dangerText to colors.dangerSubtle()
    }
    GroupedRow(
        title = look.title,
        supporting = look.detail,
        leading = { GlyphTile(icon = look.icon, tint = ink, container = wash) },
        trailing = { KickerText(text = look.tag, color = ink) },
        modifier = Modifier
            .semantics(mergeDescendants = true) {}
            .testTag(SettingsScreenTestTags.DELIVERY_STATUS),
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
    val colors = Waddle.colors
    val avatarLabel = stringResource(R.string.settings_avatar)
    val avatarModifier = Modifier
        .size(AvatarSize.LG.dp)
        .clip(CircleShape)
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onOpenProfile)
            .padding(Waddle.space.md)
            .testTag(SettingsScreenTestTags.ACCOUNT_ROW),
    ) {
        when {
            xmppAvatar != null -> Image(
                bitmap = xmppAvatar,
                contentDescription = avatarLabel,
                contentScale = ContentScale.Crop,
                modifier = avatarModifier,
            )
            state.avatarUrl != null -> AsyncImage(
                model = state.avatarUrl,
                contentDescription = avatarLabel,
                contentScale = ContentScale.Crop,
                modifier = avatarModifier,
            )
            // No image yet: initials, like every other person surface.
            else -> PeerAvatar(
                jid = null,
                displayName = state.username ?: state.jid?.substringBefore('@').orEmpty(),
                size = AvatarSize.LG.dp,
                modifier = Modifier.semantics { contentDescription = avatarLabel },
            )
        }
        Column(modifier = Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(Waddle.space.xxs)) {
            Text(
                text = state.username ?: stringResource(R.string.settings_account),
                style = MaterialTheme.typography.titleMedium,
                color = colors.fg,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            state.jid?.let {
                Text(
                    text = it,
                    style = Waddle.type.time,
                    color = colors.fgMuted,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
        RowChevron()
    }
}

/** Night or Daylight, as Material 3 Expressive's connected button group. */
@Composable
private fun ThemeSelector(selected: ThemeMode, onSelect: (ThemeMode) -> Unit) {
    SegmentedChoice(
        options = listOf(
            ChoiceOption(ThemeMode.NIGHT, stringResource(R.string.settings_theme_night), Icons.Outlined.DarkMode),
            ChoiceOption(
                ThemeMode.DAYLIGHT,
                stringResource(R.string.settings_theme_daylight),
                Icons.Outlined.LightMode,
            ),
        ),
        selected = selected,
        onSelect = onSelect,
        modifier = Modifier.padding(Waddle.space.md),
    )
}

@Composable
private fun ToggleRow(
    title: String,
    checked: Boolean,
    onCheckedChange: (Boolean) -> Unit,
    toggleTestTag: String,
    supporting: String? = null,
) {
    GroupedRow(
        title = title,
        supporting = supporting,
        trailing = {
            Switch(
                checked = checked,
                onCheckedChange = onCheckedChange,
                colors = huddleSwitchColors(),
                modifier = Modifier.testTag(toggleTestTag),
            )
        },
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
    const val WALLPAPER_COLORS_TOGGLE = "settings-wallpaper-colors-toggle"
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

    GroupedRow(
        title = stringResource(R.string.settings_battery_row_title),
        supporting = stringResource(
            if (exempt) R.string.settings_battery_exempt else R.string.settings_battery_not_exempt,
        ),
        trailing = {
            if (!exempt) {
                TextButton(onClick = { requestBatteryExemption(context) }) {
                    Text(text = stringResource(R.string.settings_battery_request), color = Waddle.colors.actionText)
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

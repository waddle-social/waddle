package social.waddle.android.feature.you

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.AccountCircle
import androidx.compose.material.icons.outlined.Settings
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.ScaffoldDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.client.ConnectionState
import social.waddle.android.client.store.PresenceShow
import social.waddle.android.feature.profile.formatPepKeyword
import social.waddle.android.jid.bareJidOf
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.GlyphTile
import social.waddle.android.ui.GroupDivider
import social.waddle.android.ui.GroupedRow
import social.waddle.android.ui.GroupedSection
import social.waddle.android.ui.KickerText
import social.waddle.android.ui.RowChevron
import social.waddle.android.ui.WaddleTopBar
import social.waddle.android.ui.huddle.AvatarSize
import social.waddle.android.ui.huddle.HuddleAvatar
import social.waddle.android.ui.huddle.HuddleCard

/** The You tab: who you are and how you show up, then profile and settings. */
@Composable
fun YouScreen(
    onOpenProfile: () -> Unit,
    onOpenSettings: () -> Unit,
    contentWindowInsets: WindowInsets = ScaffoldDefaults.contentWindowInsets,
) {
    val graph = LocalAppGraph.current
    val session by graph.currentSession.collectAsStateWithLifecycle()
    val vcard by graph.sessionManager.profileStore.selfVcard.collectAsStateWithLifecycle()
    val mood by graph.sessionManager.profileStore.selfMood.collectAsStateWithLifecycle()
    val connection by graph.sessionManager.connectionState.collectAsStateWithLifecycle()
    val jid = session?.jid?.let(::bareJidOf)
    val name = vcard?.fullName?.takeIf { it.isNotBlank() }
        ?: vcard?.nickname?.takeIf { it.isNotBlank() }
        ?: session?.username
        ?: jid.orEmpty()
    YouContent(
        identity = YouIdentity(name = name, jid = jid, connection = connection, mood = mood),
        onOpenProfile = onOpenProfile,
        onOpenSettings = onOpenSettings,
        contentWindowInsets = contentWindowInsets,
    )
}

/** Stateless You tab. */
@Composable
internal fun YouContent(
    identity: YouIdentity,
    onOpenProfile: () -> Unit,
    onOpenSettings: () -> Unit,
    contentWindowInsets: WindowInsets = ScaffoldDefaults.contentWindowInsets,
) {
    Scaffold(
        contentWindowInsets = contentWindowInsets,
        containerColor = Waddle.colors.bg,
        topBar = { WaddleTopBar(title = { Text(text = stringResource(R.string.you_title)) }) },
    ) { padding ->
        Column(
            verticalArrangement = Arrangement.spacedBy(Waddle.space.xs),
            modifier = Modifier
                .fillMaxSize()
                .consumeWindowInsets(padding)
                .verticalScroll(rememberScrollState())
                .padding(padding)
                .padding(bottom = Waddle.space.xl),
        ) {
            IdentityCard(identity = identity, onClick = onOpenProfile)
            GroupedSection(title = null, modifier = Modifier.padding(top = Waddle.space.md)) {
                GroupedRow(
                    title = stringResource(R.string.you_profile),
                    supporting = stringResource(R.string.you_profile_body),
                    leading = { GlyphTile(Icons.Outlined.AccountCircle) },
                    trailing = { RowChevron() },
                    onClick = onOpenProfile,
                )
                GroupDivider()
                GroupedRow(
                    title = stringResource(R.string.home_settings),
                    supporting = stringResource(R.string.you_settings_body),
                    leading = { GlyphTile(Icons.Outlined.Settings) },
                    trailing = { RowChevron() },
                    onClick = onOpenSettings,
                )
            }
        }
    }
}

/**
 * The identity card: your avatar with your presence as others see it in
 * your rooms (here while connected), your name and address, and your
 * mood when you have published one.
 */
@Composable
private fun IdentityCard(identity: YouIdentity, onClick: () -> Unit) {
    val colors = Waddle.colors
    HuddleCard(
        onClick = onClick,
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = Waddle.space.md)
            .padding(top = Waddle.space.xs),
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Waddle.space.md),
        ) {
            HuddleAvatar(
                jid = identity.jid,
                name = identity.name,
                size = AvatarSize.XL,
                presence = presenceOf(identity.connection),
                ground = colors.surface,
            )
            Column(
                verticalArrangement = Arrangement.spacedBy(Waddle.space.xxs),
                modifier = Modifier.weight(1f),
            ) {
                Text(
                    text = identity.name,
                    style = MaterialTheme.typography.headlineSmall,
                    color = colors.fg,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.semantics { heading() },
                )
                identity.jid?.let {
                    Text(
                        text = it,
                        style = Waddle.type.time,
                        color = colors.fgMuted,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
                Text(
                    text = stringResource(connectionLabelOf(identity.connection)),
                    style = Waddle.type.control.copy(fontWeight = null),
                    color = colors.fgMuted,
                )
            }
        }
        identity.mood?.let { mood ->
            HorizontalDivider(
                color = colors.borderSoft,
                modifier = Modifier.padding(vertical = Waddle.space.md),
            )
            Column(verticalArrangement = Arrangement.spacedBy(Waddle.space.xxs)) {
                KickerText(text = stringResource(R.string.profile_mood_section))
                Text(
                    text = listOfNotNull(formatPepKeyword(mood.kind), mood.text?.takeIf(String::isNotBlank))
                        .joinToString(" · "),
                    style = Waddle.type.body,
                    color = colors.fg,
                )
            }
        }
    }
}

/**
 * Your presence as your rooms see it: joining a room is being here, so
 * a ready stream shows the green dot; a lost one shows hollow; while
 * connecting nothing is claimed.
 */
private fun presenceOf(connection: ConnectionState): PresenceShow? = when (connection) {
    ConnectionState.Ready -> PresenceShow.AVAILABLE
    ConnectionState.Offline, ConnectionState.AuthFailed, ConnectionState.Paused -> PresenceShow.OFFLINE
    else -> null
}

private fun connectionLabelOf(connection: ConnectionState): Int = when (connection) {
    ConnectionState.Ready -> R.string.huddle_you_connected
    ConnectionState.Connecting, is ConnectionState.Reconnecting -> R.string.huddle_you_connecting
    ConnectionState.Paused -> R.string.huddle_you_paused
    ConnectionState.Offline, ConnectionState.AuthFailed, ConnectionState.Idle -> R.string.huddle_you_offline
}

package social.waddle.android.feature.you

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material.icons.outlined.AccountCircle
import androidx.compose.material.icons.outlined.Settings
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.ScaffoldDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.avatar.PeerAvatar
import social.waddle.android.jid.bareJidOf
import social.waddle.android.theme.MonoMeta
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.GlyphTile
import social.waddle.android.ui.WaddleTopBar

/** The You tab: who you are, then the way into profile and settings. */
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
    val jid = session?.jid?.let(::bareJidOf)
    val name = vcard?.fullName?.takeIf { it.isNotBlank() }
        ?: vcard?.nickname?.takeIf { it.isNotBlank() }
        ?: session?.username
        ?: jid.orEmpty()
    Scaffold(
        contentWindowInsets = contentWindowInsets,
        topBar = { WaddleTopBar(title = { Text(text = stringResource(R.string.you_title)) }) },
    ) { padding ->
        Column(
            modifier = Modifier
                .padding(padding)
                .consumeWindowInsets(padding)
                .verticalScroll(rememberScrollState()),
        ) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(Waddle.space.md),
                modifier = Modifier
                    .fillMaxWidth()
                    .clickable(onClick = onOpenProfile)
                    .padding(Waddle.space.md),
            ) {
                PeerAvatar(jid = jid, displayName = name, size = 64.dp)
                Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
                    Text(
                        text = name,
                        style = MaterialTheme.typography.titleLarge,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    jid?.let {
                        Text(text = it, style = MonoMeta, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    }
                    mood?.let { current ->
                        Text(
                            text = listOfNotNull(current.kind, current.text).joinToString(" · "),
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            }
            YouRow(
                icon = Icons.Outlined.AccountCircle,
                title = stringResource(R.string.you_profile),
                body = stringResource(R.string.you_profile_body),
                onClick = onOpenProfile,
            )
            YouRow(
                icon = Icons.Outlined.Settings,
                title = stringResource(R.string.home_settings),
                body = stringResource(R.string.you_settings_body),
                onClick = onOpenSettings,
            )
        }
    }
}

@Composable
private fun YouRow(icon: ImageVector, title: String, body: String, onClick: () -> Unit) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            .padding(horizontal = Waddle.space.md, vertical = Waddle.space.sm),
    ) {
        GlyphTile(icon)
        Column(modifier = Modifier.weight(1f)) {
            Text(text = title, style = MaterialTheme.typography.bodyLarge)
            Text(
                text = body,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        Icon(
            Icons.AutoMirrored.Filled.KeyboardArrowRight,
            contentDescription = null,
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.size(20.dp),
        )
    }
}

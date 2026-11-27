package social.waddle.android.feature.account

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.Check
import androidx.compose.material.icons.outlined.MoreVert
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.launch
import social.waddle.android.AccountTile
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.client.ConnectionState
import social.waddle.android.theme.Waddle

/**
 * The account switcher: every signed-in account with its connection and
 * unread state, tap to show one, "Add account" at the end. Switching is
 * instant because each account stays connected.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AccountSheet(onDismiss: () -> Unit) {
    val graph = LocalAppGraph.current
    val tiles by graph.accountTiles.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()
    var confirmSignOut by remember { mutableStateOf<AccountTile?>(null) }
    ModalBottomSheet(onDismissRequest = onDismiss, containerColor = Waddle.colors.surface) {
        Column(
            modifier = Modifier
                .testTag(AccountSheetTestTags.SHEET)
                .navigationBarsPadding()
                .padding(bottom = Waddle.space.md),
        ) {
            Text(
                text = stringResource(R.string.accounts_title),
                style = Waddle.type.title,
                color = Waddle.colors.fg,
                modifier = Modifier.padding(horizontal = Waddle.space.lg, vertical = Waddle.space.sm),
            )
            tiles.forEach { tile ->
                AccountRow(
                    tile = tile,
                    onSelect = {
                        scope.launch { graph.selectAccount(tile.key) }
                        onDismiss()
                    },
                    onSignOut = { confirmSignOut = tile },
                )
            }
            AddRow(
                onClick = {
                    onDismiss()
                    graph.beginAddAccount()
                },
            )
        }
    }
    confirmSignOut?.let { tile ->
        AlertDialog(
            onDismissRequest = { confirmSignOut = null },
            title = { Text(stringResource(R.string.accounts_sign_out_title, tile.label)) },
            text = { Text(stringResource(R.string.accounts_sign_out_body, tile.jid)) },
            confirmButton = {
                TextButton(
                    onClick = {
                        confirmSignOut = null
                        onDismiss()
                        scope.launch { graph.signOut(tile.key) }
                    },
                ) { Text(stringResource(R.string.accounts_sign_out), color = Waddle.colors.dangerText) }
            },
            dismissButton = {
                TextButton(onClick = { confirmSignOut = null }) { Text(stringResource(R.string.accounts_cancel)) }
            },
        )
    }
}

@Composable
private fun AccountRow(tile: AccountTile, onSelect: () -> Unit, onSignOut: () -> Unit) {
    val colors = Waddle.colors
    val status = statusText(tile)
    val rowDescription = stringResource(R.string.accounts_row_description, tile.label, tile.jid)
    var menuOpen by remember { mutableStateOf(false) }
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.md),
        modifier = Modifier
            .testTag(AccountSheetTestTags.ROW_PREFIX + tile.key)
            .clickable(onClick = onSelect)
            .semantics(mergeDescendants = true) { contentDescription = rowDescription }
            .padding(horizontal = Waddle.space.lg, vertical = Waddle.space.sm),
    ) {
        Box(contentAlignment = Alignment.BottomEnd) {
            Box(
                contentAlignment = Alignment.Center,
                modifier = Modifier.size(AVATAR).clip(CircleShape).background(colors.actionSubtle),
            ) {
                Text(
                    text = tile.label.take(1).uppercase(),
                    style = Waddle.type.title,
                    color = colors.actionText,
                )
            }
            Box(
                modifier = Modifier
                    .size(STATUS_DOT)
                    .clip(CircleShape)
                    .background(connectionColor(tile)),
            )
        }
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = tile.label,
                style = Waddle.type.title,
                color = colors.fg,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                text = tile.jid,
                style = Waddle.type.time,
                color = colors.fgMuted,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            status?.let {
                Text(
                    text = it,
                    style = Waddle.type.time,
                    color = if (tile.needsSignIn) colors.warningText else colors.fgSoft,
                )
            }
        }
        if (tile.unread > 0 && !tile.selected) {
            val unread = pluralStringResource(R.plurals.accounts_unread_count, tile.unread, tile.unread)
            Text(
                text = if (tile.unread > MAX_BADGE) "$MAX_BADGE+" else tile.unread.toString(),
                style = Waddle.type.time,
                color = colors.actionFg,
                modifier = Modifier
                    .semantics { contentDescription = unread }
                    .clip(CircleShape)
                    .background(colors.action)
                    .padding(horizontal = Waddle.space.xs),
            )
        }
        if (tile.selected) {
            Icon(Icons.Outlined.Check, contentDescription = null, tint = colors.actionText)
        }
        Box {
            IconButton(onClick = { menuOpen = true }) {
                Icon(
                    Icons.Outlined.MoreVert,
                    contentDescription = stringResource(R.string.accounts_more, tile.label),
                    tint = colors.fgMuted,
                )
            }
            DropdownMenu(expanded = menuOpen, onDismissRequest = { menuOpen = false }) {
                DropdownMenuItem(
                    text = { Text(stringResource(R.string.accounts_sign_out), color = colors.dangerText) },
                    onClick = {
                        menuOpen = false
                        onSignOut()
                    },
                )
            }
        }
    }
}

@Composable
private fun AddRow(onClick: () -> Unit) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.md),
        modifier = Modifier
            .testTag(AccountSheetTestTags.ADD)
            .clickable(onClick = onClick)
            .padding(horizontal = Waddle.space.lg, vertical = Waddle.space.md),
    ) {
        Icon(Icons.Outlined.Add, contentDescription = null, tint = Waddle.colors.actionText)
        Text(
            text = stringResource(R.string.accounts_add),
            style = Waddle.type.title,
            color = Waddle.colors.actionText,
        )
    }
}

@Composable
private fun statusText(tile: AccountTile): String? = when {
    tile.needsSignIn -> stringResource(R.string.accounts_needs_sign_in)
    tile.paused -> stringResource(R.string.accounts_paused)
    else -> when (tile.connection) {
        ConnectionState.Ready -> stringResource(R.string.accounts_state_connected)
        ConnectionState.Connecting,
        is ConnectionState.Reconnecting,
        -> stringResource(R.string.accounts_state_connecting)
        ConnectionState.Offline -> stringResource(R.string.accounts_state_offline)
        ConnectionState.AuthFailed -> stringResource(R.string.accounts_needs_sign_in)
        // Idle and Paused (push mode) are normal states: no label.
        ConnectionState.Idle, ConnectionState.Paused -> null
    }
}

/** The status ring: green connected, amber connecting, grey offline or paused, red needs sign-in. */
@Composable
private fun connectionColor(tile: AccountTile) = when {
    tile.needsSignIn || tile.connection == ConnectionState.AuthFailed -> Waddle.colors.danger
    tile.paused -> Waddle.colors.fgSoft
    else -> when (tile.connection) {
        ConnectionState.Ready -> Waddle.colors.success
        ConnectionState.Connecting, is ConnectionState.Reconnecting -> Waddle.colors.warning
        else -> Waddle.colors.fgSoft
    }
}

private val AVATAR = 40.dp
private val STATUS_DOT = 12.dp
private const val MAX_BADGE = 99

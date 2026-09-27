package social.waddle.android.feature.conversation

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import social.waddle.android.R
import social.waddle.android.avatar.PeerAvatar

/**
 * "Alice is typing…" line above the composer, led by the typists'
 * avatars ([avatarJidOf] resolves a name to a real bare JID, `null` =
 * initials); gone when nobody is.
 */
@Composable
fun TypingIndicator(
    names: List<String>,
    modifier: Modifier = Modifier,
    avatarJidOf: (String) -> String? = { null },
) {
    if (names.isEmpty()) return
    val text = if (names.size == 1) {
        stringResource(R.string.typing_one, names.single())
    } else {
        stringResource(R.string.typing_many, names.joinToString())
    }
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(2.dp),
        modifier = modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 2.dp),
    ) {
        names.take(MAX_TYPING_AVATARS).forEach { name ->
            PeerAvatar(jid = avatarJidOf(name), displayName = name, size = 16.dp)
        }
        Text(
            text = text,
            style = MaterialTheme.typography.labelMedium,
            fontStyle = FontStyle.Italic,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.padding(start = 4.dp),
        )
    }
}

private const val MAX_TYPING_AVATARS = 3

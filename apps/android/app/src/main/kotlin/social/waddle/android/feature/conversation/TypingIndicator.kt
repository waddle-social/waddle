package social.waddle.android.feature.conversation

import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import social.waddle.android.R

/** "Alice is typing…" line above the composer; gone when nobody is. */
@Composable
fun TypingIndicator(names: List<String>, modifier: Modifier = Modifier) {
    if (names.isEmpty()) return
    val text = if (names.size == 1) {
        stringResource(R.string.typing_one, names.single())
    } else {
        stringResource(R.string.typing_many, names.joinToString())
    }
    Text(
        text = text,
        style = MaterialTheme.typography.labelMedium,
        fontStyle = FontStyle.Italic,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        maxLines = 1,
        overflow = TextOverflow.Ellipsis,
        modifier = modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 2.dp),
    )
}

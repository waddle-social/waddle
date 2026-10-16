package social.waddle.android.feature.conversation

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import social.waddle.android.R
import social.waddle.android.client.ExtensionCommand
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.huddle.Kicker

/**
 * `/` autocomplete candidates in a popover above the composer
 * ([MentionPopover] sibling); tap completes the prefix. The close
 * affordance feeds the composer's per-prefix dismissal memory —
 * dismissed for this prefix, it stays hidden until the prefix
 * changes.
 */
@Composable
fun SlashCommandPopover(
    candidates: List<ExtensionCommand>,
    onSelect: (ExtensionCommand) -> Unit,
    onDismiss: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = Waddle.colors
    PopoverSurface(modifier = modifier.testTag(SlashCommandTestTags.POPOVER)) {
        Column(
            modifier = Modifier
                .heightIn(max = POPOVER_MAX_HEIGHT)
                .verticalScroll(rememberScrollState()),
        ) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(start = Waddle.space.md, end = Waddle.space.xxs),
            ) {
                Kicker(
                    text = stringResource(R.string.slash_commands_title),
                    heading = true,
                    modifier = Modifier.weight(1f),
                )
                IconButton(
                    onClick = onDismiss,
                    modifier = Modifier.testTag(SlashCommandTestTags.DISMISS),
                ) {
                    Icon(
                        Icons.Outlined.Close,
                        contentDescription = stringResource(R.string.slash_commands_dismiss),
                        tint = colors.fgMuted,
                    )
                }
            }
            candidates.forEach { command ->
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
                    modifier = Modifier
                        .fillMaxWidth()
                        .heightIn(min = Waddle.space.xxl)
                        .clickable { onSelect(command) }
                        .padding(horizontal = Waddle.space.md)
                        .testTag(SlashCommandTestTags.item(command.node)),
                ) {
                    Text(
                        text = "/${command.composerPrefix.orEmpty()}",
                        style = Waddle.type.time.copy(fontSize = Waddle.type.control.fontSize),
                        color = colors.actionText,
                        maxLines = 1,
                    )
                    Text(
                        text = command.name,
                        style = Waddle.type.control,
                        color = colors.fgMuted,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
        }
    }
}

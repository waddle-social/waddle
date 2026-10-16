package social.waddle.android.feature.conversation

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Campaign
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.key
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import social.waddle.android.client.MentionCandidate
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.huddle.AvatarSize
import social.waddle.android.ui.huddle.HuddleAvatar

/**
 * `@` autocomplete candidates in a popover above the composer; tap
 * inserts the mention. Broadcast rows (`@everyone`, `@here`) lead with a
 * teal megaphone to signal their room-wide reach.
 */
@Composable
fun MentionPopover(
    candidates: List<MentionCandidate>,
    onSelect: (MentionCandidate) -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = Waddle.colors
    PopoverSurface(modifier = modifier) {
        Column(
            modifier = Modifier
                .heightIn(max = POPOVER_MAX_HEIGHT)
                .verticalScroll(rememberScrollState())
                .padding(vertical = Waddle.space.xxs),
        ) {
            candidates.forEach { candidate ->
                key(candidate.uri) {
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
                        modifier = Modifier
                            .fillMaxWidth()
                            .heightIn(min = Waddle.space.xxl)
                            .clickable { onSelect(candidate) }
                            .padding(horizontal = Waddle.space.md),
                    ) {
                        if (candidate.isBroadcast) {
                            Box(
                                contentAlignment = Alignment.Center,
                                modifier = Modifier
                                    .size(AvatarSize.XS.dp)
                                    .background(colors.actionSubtle, CircleShape),
                            ) {
                                Icon(
                                    Icons.Outlined.Campaign,
                                    contentDescription = null,
                                    tint = colors.actionText,
                                    modifier = Modifier.size(BROADCAST_ICON),
                                )
                            }
                        } else {
                            HuddleAvatar(
                                jid = candidate.jid,
                                name = candidate.display,
                                size = AvatarSize.XS,
                                ground = colors.surface,
                            )
                        }
                        Text(
                            text = "@${candidate.display}",
                            style = Waddle.type.control,
                            color = if (candidate.isBroadcast) colors.actionText else colors.fg,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                }
            }
        }
    }
}

internal val POPOVER_MAX_HEIGHT = 240.dp
private val BROADCAST_ICON = 14.dp

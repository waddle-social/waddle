package social.waddle.android.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.defaultMinSize
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.NotificationsOff
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.em
import social.waddle.android.R
import social.waddle.android.theme.MonoMeta
import social.waddle.android.theme.Waddle
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle
import java.util.Locale

/**
 * The app's top bar: the screen ground (no tonal tint), Space Grotesk
 * title, hairline-free. Every screen uses it so titles line up.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun WaddleTopBar(
    title: @Composable () -> Unit,
    modifier: Modifier = Modifier,
    navigationIcon: @Composable () -> Unit = {},
    actions: @Composable RowScope.() -> Unit = {},
) {
    TopAppBar(
        title = title,
        navigationIcon = navigationIcon,
        actions = actions,
        modifier = modifier,
        colors = TopAppBarDefaults.topAppBarColors(
            containerColor = MaterialTheme.colorScheme.background,
            scrolledContainerColor = MaterialTheme.colorScheme.surfaceContainer,
        ),
    )
}

/** Unread count capsule: neutral by default, accent only when it is for you. */
@Composable
fun UnreadCountBadge(count: Int, modifier: Modifier = Modifier, emphasized: Boolean = false) {
    if (count <= 0) return
    val colors = Waddle.colors
    Box(
        contentAlignment = Alignment.Center,
        modifier = modifier
            .defaultMinSize(minWidth = 20.dp, minHeight = 20.dp)
            .background(
                color = if (emphasized) MaterialTheme.colorScheme.primary else colors.neutralBadge,
                shape = CircleShape,
            )
            .padding(horizontal = 6.dp),
    ) {
        Text(
            text = if (count > MAX_BADGE) "$MAX_BADGE+" else count.toString(),
            style = MonoMeta,
            fontWeight = FontWeight.Medium,
            color = if (emphasized) MaterialTheme.colorScheme.onPrimary else colors.onNeutralBadge,
        )
    }
}

/** Quiet uppercase section kicker (web `.type-kicker`). */
@Composable
fun SectionKicker(text: String, modifier: Modifier = Modifier) {
    Text(
        text = text.uppercase(Locale.getDefault()),
        style = MaterialTheme.typography.labelSmall.copy(letterSpacing = 0.08.em),
        fontWeight = FontWeight.SemiBold,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = modifier.padding(
            start = Waddle.space.md,
            end = Waddle.space.md,
            top = Waddle.space.lg,
            bottom = Waddle.space.xs,
        ),
    )
}

/**
 * One conversation in a list (channels, DMs, activity): leading glyph or
 * avatar, title, optional one-line preview, time, mute and unread. Unread
 * rows read heavier; read rows recede.
 */
@Composable
fun ConversationListRow(
    title: String,
    leading: @Composable () -> Unit,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    preview: String? = null,
    time: String? = null,
    unreadCount: Int = 0,
    isMuted: Boolean = false,
    trailingExtra: @Composable () -> Unit = {},
) {
    val unread = unreadCount > 0 && !isMuted
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
        modifier = modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            .heightIn(min = if (preview != null) 64.dp else 48.dp)
            .padding(horizontal = Waddle.space.md, vertical = Waddle.space.xs),
    ) {
        leading()
        Column(modifier = Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    text = title,
                    style = MaterialTheme.typography.bodyLarge,
                    fontWeight = if (unread) FontWeight.SemiBold else FontWeight.Normal,
                    color = if (unread || preview != null) {
                        MaterialTheme.colorScheme.onSurface
                    } else {
                        MaterialTheme.colorScheme.onSurfaceVariant
                    },
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f),
                )
                time?.let {
                    Text(
                        text = it,
                        style = MonoMeta,
                        color = if (unread) {
                            MaterialTheme.colorScheme.primary
                        } else {
                            MaterialTheme.colorScheme.onSurfaceVariant
                        },
                        modifier = Modifier.padding(start = Waddle.space.xs),
                    )
                }
            }
            if (preview != null) {
                Text(
                    text = preview,
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs),
        ) {
            trailingExtra()
            if (isMuted) {
                Icon(
                    Icons.Outlined.NotificationsOff,
                    contentDescription = stringResource(R.string.notify_muted_badge),
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.size(16.dp),
                )
            }
            UnreadCountBadge(count = unreadCount, emphasized = false)
        }
    }
}

/** Rounded square glyph tile in place of an avatar (channels, groups). */
@Composable
fun GlyphTile(icon: ImageVector, modifier: Modifier = Modifier, size: Dp = 40.dp) {
    Box(
        contentAlignment = Alignment.Center,
        modifier = modifier
            .size(size)
            .background(MaterialTheme.colorScheme.surfaceContainerHigh, RoundedCornerShape(Waddle.radius.md)),
    ) {
        Icon(
            icon,
            contentDescription = null,
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.size(size / 2),
        )
    }
}

/** Centered empty state: a glyph, a title and one line of guidance. */
@Composable
fun EmptyState(
    icon: ImageVector,
    title: String,
    body: String,
    modifier: Modifier = Modifier,
    action: @Composable () -> Unit = {},
) {
    Column(
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(Waddle.space.sm, Alignment.CenterVertically),
        modifier = modifier
            .fillMaxSize()
            .padding(Waddle.space.xl),
    ) {
        GlyphTile(icon = icon, size = 56.dp)
        Text(
            text = title,
            style = MaterialTheme.typography.titleMedium,
            textAlign = TextAlign.Center,
        )
        Text(
            text = body,
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            textAlign = TextAlign.Center,
        )
        action()
    }
}

/** List time: clock time today, weekday this week, else a short date. */
fun listTimeOf(
    instant: Instant,
    zone: ZoneId = ZoneId.systemDefault(),
    today: LocalDate = LocalDate.now(zone),
): String {
    val day = instant.atZone(zone).toLocalDate()
    return when {
        day == today -> DateTimeFormatter.ofLocalizedTime(FormatStyle.SHORT).withZone(zone).format(instant)
        day.isAfter(today.minusDays(WEEK_DAYS)) -> DateTimeFormatter.ofPattern("EEE").withZone(zone).format(instant)
        else -> DateTimeFormatter.ofPattern("d MMM").withZone(zone).format(instant)
    }
}

private const val MAX_BADGE = 99
private const val WEEK_DAYS = 6L

package social.waddle.android.navigation

import androidx.compose.animation.animateColorAsState
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.material3.BadgedBox
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import social.waddle.android.R
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.huddle.CountBadge
import social.waddle.android.ui.huddle.HuddleMotion
import social.waddle.android.ui.huddle.LiveDot

/**
 * Huddle's bottom tabs (web phone shell): the rail colour with a 1dp
 * top border, no indicator pill; the active tab's icon and label turn
 * teal. DMs carry a neutral unread count, Rooms an ember dot while a
 * huddle is live. 64dp of tabs above the navigation bar.
 */
@Composable
fun MainTabBar(
    selected: MainTab,
    badges: TabBadges,
    onSelect: (MainTab) -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = Waddle.colors
    val haptics = LocalHapticFeedback.current
    Column(modifier = modifier.background(colors.bgRail)) {
        Spacer(
            Modifier
                .fillMaxWidth()
                .height(1.dp)
                .background(colors.border),
        )
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .windowInsetsPadding(WindowInsets.navigationBars)
                .heightIn(min = BAR_HEIGHT)
                .selectableGroup(),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            MainTab.entries.forEach { tab ->
                TabItem(
                    tab = tab,
                    selected = tab == selected,
                    badges = badges,
                    onClick = {
                        if (tab != selected) haptics.performHapticFeedback(HapticFeedbackType.SegmentTick)
                        onSelect(tab)
                    },
                    modifier = Modifier.weight(1f),
                )
            }
        }
    }
}

@Composable
private fun TabItem(
    tab: MainTab,
    selected: Boolean,
    badges: TabBadges,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = Waddle.colors
    val tint by animateColorAsState(
        targetValue = if (selected) colors.actionText else colors.fgMuted,
        animationSpec = HuddleMotion.fast(),
        label = "tab-tint",
    )
    Column(
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(Waddle.space.xxs, Alignment.CenterVertically),
        modifier = modifier
            .heightIn(min = BAR_HEIGHT)
            .selectable(selected = selected, role = Role.Tab, onClick = onClick)
            .testTag(tab.testTag),
    ) {
        BadgedBox(badge = { TabBadge(tab = tab, badges = badges) }) {
            Icon(if (selected) tab.selectedIcon else tab.icon, contentDescription = null, tint = tint)
        }
        Text(
            text = stringResource(tab.label),
            style = MaterialTheme.typography.labelSmall,
            color = tint,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

/** The DMs count or the Rooms live dot, announced in words rather than as a bare number. */
@Composable
private fun TabBadge(tab: MainTab, badges: TabBadges, modifier: Modifier = Modifier) {
    when {
        tab == MainTab.DMS && badges.dmUnread > 0 -> {
            val description = pluralStringResource(R.plurals.tab_unread_badge, badges.dmUnread, badges.dmUnread)
            CountBadge(
                count = badges.dmUnread,
                modifier = modifier.clearAndSetSemantics { contentDescription = description },
            )
        }
        tab == MainTab.ROOMS && badges.roomsLive -> {
            val description = stringResource(R.string.tab_rooms_live)
            LiveDot(modifier = modifier.clearAndSetSemantics { contentDescription = description })
        }
    }
}

private val BAR_HEIGHT = 64.dp

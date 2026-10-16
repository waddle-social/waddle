package social.waddle.android.feature.people

import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.ExpandMore
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.ScaffoldDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.client.store.PresenceShow
import social.waddle.android.feature.home.DashedNote
import social.waddle.android.feature.home.PageHeader
import social.waddle.android.feature.home.SectionLabel
import social.waddle.android.feature.home.ShellSearchField
import social.waddle.android.feature.home.rememberStableOrder
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.huddle.AvatarSize
import social.waddle.android.ui.huddle.HuddleAvatar
import social.waddle.android.ui.huddle.HuddleMotion
import social.waddle.android.ui.huddle.SkeletonBlock
import social.waddle.android.ui.huddle.SpeakingBars

/**
 * The People tab (web people rail): who is in a huddle, who is around,
 * and — folded away — who is away or offline. Tapping someone opens the
 * conversation with them. Order holds still while you look.
 */
@Composable
fun PeopleScreen(
    onOpenDm: (peerJid: String, name: String) -> Unit,
    contentWindowInsets: WindowInsets = ScaffoldDefaults.contentWindowInsets,
    speaking: Set<String> = emptySet(),
) {
    val graph = LocalAppGraph.current
    val viewModel: PeopleViewModel = viewModel(factory = PeopleViewModel.factory(graph))
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    var query by rememberSaveable { mutableStateOf("") }
    var awayOpen by rememberSaveable { mutableStateOf(false) }
    Scaffold(contentWindowInsets = contentWindowInsets) { padding ->
        PeopleContent(
            people = personRowsOf(state.people, state.roomNames, speaking),
            settled = state.settled,
            query = query,
            onQueryChange = { query = it },
            awayOpen = awayOpen,
            onToggleAway = { awayOpen = !awayOpen },
            onOpen = { person -> onOpenDm(person.jid, person.name) },
            modifier = Modifier
                .padding(padding)
                .consumeWindowInsets(padding),
        )
    }
}

/** Semantics tags shared with instrumented tests. */
object PeopleScreenTestTags {
    const val SCREEN = "people-screen"
    const val AWAY_TOGGLE = "people-away-toggle"
}

/** The People tab body, stateless. */
@Composable
fun PeopleContent(
    people: List<PersonRow>,
    settled: Boolean,
    query: String,
    onQueryChange: (String) -> Unit,
    awayOpen: Boolean,
    onToggleAway: () -> Unit,
    onOpen: (PersonRow) -> Unit,
    modifier: Modifier = Modifier,
) {
    val listState = rememberLazyListState()
    val ordered = rememberStableOrder(
        sorted = people.sortedWith(PersonOrder),
        keyOf = PersonRow::jid,
        interacting = listState.isScrollInProgress,
    )
    // Groups are live (someone who joins a huddle moves up a section);
    // inside a group, people keep the place they had.
    val rank = ordered.withIndex().associate { (index, person) -> person.jid to index }
    val groups = peopleGroupsOf(people, query).mapValues { (_, members) ->
        members.sortedBy { rank[it.jid] ?: Int.MAX_VALUE }
    }
    val needle = query.trim()
    LazyColumn(
        state = listState,
        contentPadding = PaddingValues(
            start = Waddle.space.md,
            end = Waddle.space.md,
            top = Waddle.space.lg,
            bottom = Waddle.space.xl,
        ),
        modifier = modifier
            .fillMaxSize()
            .imePadding()
            .testTag(PeopleScreenTestTags.SCREEN),
    ) {
        item(key = "header", contentType = "header") {
            PageHeader(
                title = stringResource(R.string.people_title),
                kicker = pluralStringResource(R.plurals.people_count, people.size, people.size),
                lead = stringResource(R.string.people_lead),
            )
        }
        item(key = "search", contentType = "search") {
            ShellSearchField(
                value = query,
                onValueChange = onQueryChange,
                placeholder = stringResource(R.string.people_search),
                modifier = Modifier.padding(top = Waddle.space.md),
            )
        }
        when {
            people.isEmpty() && !settled -> loadingRows()
            people.isEmpty() -> item(key = "empty") {
                DashedNote(
                    text = stringResource(R.string.people_empty),
                    modifier = Modifier.padding(top = Waddle.space.lg),
                )
            }
            groups.values.all { it.isEmpty() } -> item(key = "no-match") {
                DashedNote(
                    text = stringResource(R.string.people_no_match, needle),
                    modifier = Modifier.padding(top = Waddle.space.lg),
                )
            }
            else -> {
                // While searching, empty sections only add noise.
                val searching = needle.isNotEmpty()
                group(PeopleGroup.HUDDLE, groups.getValue(PeopleGroup.HUDDLE), searching, onOpen)
                group(PeopleGroup.AROUND, groups.getValue(PeopleGroup.AROUND), searching, onOpen)
                awayGroup(groups.getValue(PeopleGroup.AWAY), open = awayOpen || searching, onToggleAway, onOpen)
            }
        }
    }
}

private fun LazyListScope.group(
    group: PeopleGroup,
    members: List<PersonRow>,
    searching: Boolean,
    onOpen: (PersonRow) -> Unit,
) {
    if (members.isEmpty() && searching) return
    item(key = "group-$group", contentType = "group") {
        val title = stringResource(
            if (group == PeopleGroup.HUDDLE) R.string.people_group_huddle else R.string.people_group_around,
        )
        SectionLabel(
            text = if (members.isEmpty()) title else stringResource(R.string.people_group_kicker, title, members.size),
            live = group == PeopleGroup.HUDDLE && members.isNotEmpty(),
            modifier = Modifier.padding(top = Waddle.space.xl, bottom = Waddle.space.xs),
        )
    }
    if (members.isEmpty()) {
        item(key = "group-$group-empty", contentType = "group-empty") {
            Text(
                text = stringResource(
                    if (group == PeopleGroup.HUDDLE) R.string.people_empty_huddle else R.string.people_empty_around,
                ),
                style = MaterialTheme.typography.bodyMedium,
                color = Waddle.colors.fgMuted,
                modifier = Modifier.padding(vertical = Waddle.space.xs),
            )
        }
    }
    items(items = members, key = { "person-${it.jid}" }, contentType = { "person" }) { person ->
        PersonListRow(person = person, onClick = { onOpen(person) }, modifier = Modifier.animateItem())
    }
}

/** "Away and offline · N": a header that folds the quiet half of the list away. */
private fun LazyListScope.awayGroup(
    members: List<PersonRow>,
    open: Boolean,
    onToggle: () -> Unit,
    onOpen: (PersonRow) -> Unit,
) {
    if (members.isEmpty()) return
    item(key = "group-away", contentType = "group") {
        val title = stringResource(R.string.people_group_away)
        val action = stringResource(if (open) R.string.people_group_hide else R.string.people_group_show)
        val turn by animateFloatAsState(if (open) HALF_TURN else 0f, HuddleMotion.fast(), label = "away-chevron")
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier
                .padding(top = Waddle.space.xl)
                .fillMaxWidth()
                .heightIn(min = Waddle.control.touch)
                .clip(RoundedCornerShape(Waddle.radius.control))
                .clickable(onClickLabel = action, role = Role.Button, onClick = onToggle)
                .testTag(PeopleScreenTestTags.AWAY_TOGGLE),
        ) {
            SectionLabel(
                text = stringResource(R.string.people_group_kicker, title, members.size),
                modifier = Modifier.weight(1f),
            )
            Icon(
                Icons.Outlined.ExpandMore,
                contentDescription = null,
                tint = Waddle.colors.fgMuted,
                modifier = Modifier.rotate(turn),
            )
        }
    }
    if (open) {
        items(items = members, key = { "person-${it.jid}" }, contentType = { "person" }) { person ->
            PersonListRow(person = person, onClick = { onOpen(person) }, modifier = Modifier.animateItem())
        }
    }
}

/**
 * One person (web people-rail row): avatar with presence and huddle
 * ring, name, a status line (ember when they are live), speaking bars at
 * the end. The whole row is one target that opens the DM.
 */
@Composable
fun PersonListRow(person: PersonRow, onClick: () -> Unit, modifier: Modifier = Modifier) {
    val colors = Waddle.colors
    val live = person.speaking || person.inHuddle
    val message = stringResource(R.string.people_message, person.name)
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
        modifier = modifier
            .fillMaxWidth()
            .heightIn(min = ROW_HEIGHT)
            .clip(RoundedCornerShape(Waddle.radius.control))
            .clickable(onClickLabel = message, role = Role.Button, onClick = onClick)
            .padding(horizontal = Waddle.space.xs, vertical = Waddle.space.xs),
    ) {
        // The status line says it in words; the dot's own label would repeat it.
        Box(Modifier.clearAndSetSemantics { }) {
            HuddleAvatar(
                jid = person.jid,
                name = person.name,
                size = AvatarSize.MD,
                presence = person.show,
                inHuddle = person.inHuddle,
                speaking = person.speaking,
            )
        }
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = person.name,
                style = MaterialTheme.typography.bodyLarge,
                color = colors.fg,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                text = statusOf(person),
                style = MaterialTheme.typography.bodySmall,
                color = if (live) colors.liveText else colors.fgMuted,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
        if (person.speaking) SpeakingBars()
    }
}

/** What someone is doing, in words (web people-rail status). */
@Composable
fun statusOf(person: PersonRow): String = when {
    person.speaking -> stringResource(R.string.people_status_speaking)
    person.inHuddle && person.huddleRoomName != null ->
        stringResource(R.string.people_status_huddle_in, person.huddleRoomName)
    person.inHuddle -> stringResource(R.string.people_group_huddle)
    else -> when (person.show) {
        PresenceShow.AVAILABLE -> stringResource(R.string.people_status_available)
        PresenceShow.AWAY, PresenceShow.XA -> stringResource(R.string.people_status_away)
        PresenceShow.DND -> stringResource(R.string.people_status_dnd)
        PresenceShow.OFFLINE -> stringResource(R.string.people_status_offline)
    }
}

private fun LazyListScope.loadingRows() {
    item(key = "loading-label") {
        SkeletonBlock(
            Modifier
                .padding(top = Waddle.space.xl, bottom = Waddle.space.xs)
                .fillMaxWidth(SKELETON_LABEL),
        )
    }
    items(count = SKELETON_ROWS, key = { "skeleton-$it" }) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
            modifier = Modifier
                .heightIn(min = ROW_HEIGHT)
                .padding(horizontal = Waddle.space.xs),
        ) {
            SkeletonBlock(
                // SkeletonBlock insets itself vertically; give the inset back so the face stays round.
                Modifier.size(width = AvatarSize.MD.dp, height = AvatarSize.MD.dp + SKELETON_INSET * 2),
                height = AvatarSize.MD.dp,
                radius = Waddle.radius.pill,
            )
            Column(Modifier.weight(1f)) {
                SkeletonBlock(Modifier.fillMaxWidth(SKELETON_NAME))
                SkeletonBlock(Modifier.fillMaxWidth(SKELETON_STATUS))
            }
        }
    }
}

private val ROW_HEIGHT = 56.dp
private val SKELETON_INSET = 3.dp
private const val HALF_TURN = 180f
private const val SKELETON_ROWS = 6
private const val SKELETON_LABEL = 0.3f
private const val SKELETON_NAME = 0.45f
private const val SKELETON_STATUS = 0.3f

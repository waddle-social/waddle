package social.waddle.android.feature.people

import androidx.compose.foundation.background
import androidx.compose.foundation.border
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
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.GridItemSpan
import androidx.compose.foundation.lazy.grid.LazyGridScope
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.foundation.lazy.grid.rememberLazyGridState
import androidx.compose.foundation.shape.RoundedCornerShape
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
import social.waddle.android.ui.ChoiceOption
import social.waddle.android.ui.SegmentedChoice
import social.waddle.android.ui.huddle.AvatarSize
import social.waddle.android.ui.huddle.HuddleAvatar
import social.waddle.android.ui.huddle.SkeletonBlock
import social.waddle.android.ui.huddle.SpeakingBars

/**
 * The People page (web people grid): here now — in a huddle, then
 * around — or everyone, away and offline included. Tapping someone opens the
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
    var everyone by rememberSaveable { mutableStateOf(false) }
    Scaffold(contentWindowInsets = contentWindowInsets) { padding ->
        PeopleContent(
            people = personRowsOf(state.people, state.roomNames, speaking),
            settled = state.settled,
            query = query,
            onQueryChange = { query = it },
            everyone = everyone,
            onEveryoneChange = { everyone = it },
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
    const val EVERYONE_TOGGLE = "people-everyone-toggle"
}

/** The People page, stateless: Here now or Everyone, as a grid of cards. */
@Composable
fun PeopleContent(
    people: List<PersonRow>,
    settled: Boolean,
    query: String,
    onQueryChange: (String) -> Unit,
    everyone: Boolean,
    onEveryoneChange: (Boolean) -> Unit,
    onOpen: (PersonRow) -> Unit,
    modifier: Modifier = Modifier,
) {
    val gridState = rememberLazyGridState()
    val ordered = rememberStableOrder(
        sorted = people.sortedWith(PersonOrder),
        keyOf = PersonRow::jid,
        interacting = gridState.isScrollInProgress,
    )
    // Groups are live (someone who joins a huddle moves up a section);
    // inside a group, people keep the place they had.
    val rank = ordered.withIndex().associate { (index, person) -> person.jid to index }
    val groups = peopleGroupsOf(people, query).mapValues { (_, members) ->
        members.sortedBy { rank[it.jid] ?: Int.MAX_VALUE }
    }
    val needle = query.trim()
    // While searching, everyone matches.
    val showAway = everyone || needle.isNotEmpty()
    val shown = groups.filterKeys { it != PeopleGroup.AWAY || showAway }
    LazyVerticalGrid(
        columns = GridCells.Adaptive(minSize = CARD_MIN_WIDTH),
        state = gridState,
        contentPadding = PaddingValues(
            start = Waddle.space.md,
            end = Waddle.space.md,
            top = Waddle.space.xs,
            bottom = Waddle.space.xl,
        ),
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
        verticalArrangement = Arrangement.spacedBy(Waddle.space.sm),
        modifier = modifier
            .fillMaxSize()
            .imePadding()
            .testTag(PeopleScreenTestTags.SCREEN),
    ) {
        fullWidth("header") {
            PageHeader(
                title = stringResource(R.string.people_title),
                kicker = pluralStringResource(R.plurals.people_count, people.size, people.size),
                lead = stringResource(R.string.people_lead),
            )
        }
        fullWidth("filter") {
            Column(verticalArrangement = Arrangement.spacedBy(Waddle.space.sm)) {
                SegmentedChoice(
                    options = listOf(
                        ChoiceOption(false, stringResource(R.string.people_here_now)),
                        ChoiceOption(true, stringResource(R.string.people_everyone)),
                    ),
                    selected = everyone,
                    onSelect = onEveryoneChange,
                    modifier = Modifier.testTag(PeopleScreenTestTags.EVERYONE_TOGGLE),
                )
                ShellSearchField(
                    value = query,
                    onValueChange = onQueryChange,
                    placeholder = stringResource(R.string.people_search),
                )
            }
        }
        when {
            people.isEmpty() && !settled -> loadingCards()
            people.isEmpty() -> fullWidth("empty") { DashedNote(text = stringResource(R.string.people_empty)) }
            shown.values.all { it.isEmpty() } -> fullWidth("no-match") {
                DashedNote(
                    text = if (needle.isEmpty()) {
                        stringResource(R.string.people_empty_around)
                    } else {
                        stringResource(R.string.people_no_match, needle)
                    },
                )
            }
            else -> shown.forEach { (group, members) -> group(group, members, onOpen) }
        }
    }
}

private fun LazyGridScope.fullWidth(key: String, content: @Composable () -> Unit) {
    item(key = key, span = { GridItemSpan(maxLineSpan) }, contentType = key) { content() }
}

private fun LazyGridScope.group(group: PeopleGroup, members: List<PersonRow>, onOpen: (PersonRow) -> Unit) {
    if (members.isEmpty()) return
    fullWidth("group-$group") {
        val title = stringResource(
            when (group) {
                PeopleGroup.HUDDLE -> R.string.people_group_huddle
                PeopleGroup.AROUND -> R.string.people_group_around
                PeopleGroup.AWAY -> R.string.people_group_away
            },
        )
        SectionLabel(
            text = stringResource(R.string.people_group_kicker, title, members.size),
            live = group == PeopleGroup.HUDDLE,
            modifier = Modifier.padding(top = Waddle.space.md),
        )
    }
    items(items = members, key = { "person-${it.jid}" }, contentType = { "person" }) { person ->
        PersonCard(person = person, onClick = { onOpen(person) }, modifier = Modifier.animateItem())
    }
}

/**
 * One person (web people grid card): face with presence and huddle
 * ring, name, a status line (live colour when they are in a huddle),
 * speaking bars beside the face. The whole card opens the DM.
 */
@Composable
fun PersonCard(person: PersonRow, onClick: () -> Unit, modifier: Modifier = Modifier) {
    val colors = Waddle.colors
    val live = person.speaking || person.inHuddle
    val message = stringResource(R.string.people_message, person.name)
    val shape = RoundedCornerShape(Waddle.radius.panel)
    Column(
        verticalArrangement = Arrangement.spacedBy(Waddle.space.xxs),
        modifier = modifier
            .fillMaxWidth()
            .clip(shape)
            .background(colors.surface)
            .border(1.dp, colors.borderSoft, shape)
            .clickable(onClickLabel = message, role = Role.Button, onClick = onClick)
            .padding(Waddle.space.sm),
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs),
        ) {
            // The status line says it in words; the dot's own label would repeat it.
            Box(Modifier.clearAndSetSemantics { }) {
                HuddleAvatar(
                    jid = person.jid,
                    name = person.name,
                    size = AvatarSize.LG,
                    presence = person.show,
                    inHuddle = person.inHuddle,
                    speaking = person.speaking,
                    ground = colors.surface,
                )
            }
            if (person.speaking) SpeakingBars()
        }
        Text(
            text = person.name,
            style = Waddle.type.author,
            color = colors.fg,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.padding(top = Waddle.space.xxs),
        )
        Text(
            text = statusOf(person),
            style = Waddle.type.time,
            color = if (live) colors.liveText else colors.fgMuted,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
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

private fun LazyGridScope.loadingCards() {
    items(count = SKELETON_CARDS, key = { "skeleton-$it" }) {
        SkeletonBlock(Modifier.fillMaxWidth(), height = SKELETON_CARD, radius = Waddle.radius.panel)
    }
}

private val CARD_MIN_WIDTH = 150.dp
private val SKELETON_CARD = 112.dp
private const val SKELETON_CARDS = 6

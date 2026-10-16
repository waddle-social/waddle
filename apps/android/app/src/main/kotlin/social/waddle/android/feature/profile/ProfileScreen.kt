package social.waddle.android.feature.profile

import android.graphics.BitmapFactory
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.Image
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.outlined.Check
import androidx.compose.material.icons.outlined.Mood
import androidx.compose.material.icons.outlined.PhotoCamera
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExposedDropdownMenu
import androidx.compose.material3.ExposedDropdownMenuAnchorType
import androidx.compose.material3.ExposedDropdownMenuBox
import androidx.compose.material3.ExposedDropdownMenuDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import coil3.compose.AsyncImage
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.avatar.PeerAvatar
import social.waddle.android.client.GENERAL_ACTIVITIES
import social.waddle.android.client.MOOD_KINDS
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.GlyphTile
import social.waddle.android.ui.GroupedRow
import social.waddle.android.ui.GroupedSection
import social.waddle.android.ui.HuddleFieldShape
import social.waddle.android.ui.RowChevron
import social.waddle.android.ui.SheetTitle
import social.waddle.android.ui.WaddleTopBar
import social.waddle.android.ui.huddle.PillButton
import social.waddle.android.ui.huddle.PillTone
import social.waddle.android.ui.huddleFieldColors
import social.waddle.client.ffi.WaddleAvatar

/**
 * Own-profile publishing (web `UserSettingsPage` + `VCardEditor`
 * parity): XEP-0084 avatar, XEP-0292 vCard4 editor with optimistic
 * save, and the XEP-0107/0108/0118 status signal sections.
 */
@Composable
fun ProfileScreen(onBack: () -> Unit) {
    val graph = LocalAppGraph.current
    val session by graph.currentSession.collectAsStateWithLifecycle()
    // Keyed per account (channel-screen parity): logout→login as
    // another account must get a fresh VM and a fresh load instead of
    // a cached VM whose one-shot load already ran.
    val viewModel: ProfileViewModel = viewModel(
        key = "profile:${session?.jid.orEmpty()}",
        factory = ProfileViewModel.factory(graph),
    )
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    val avatar by viewModel.selfAvatar.collectAsStateWithLifecycle()
    val restAvatarUrl by viewModel.restAvatarUrl.collectAsStateWithLifecycle()
    val selfName by viewModel.selfName.collectAsStateWithLifecycle()
    val context = LocalContext.current
    val processor = remember { BitmapAvatarProcessor(context.contentResolver) }
    val pickAvatar = rememberLauncherForActivityResult(ActivityResultContracts.PickVisualMedia()) { uri ->
        if (uri != null) {
            // The whole process→publish pipeline runs in the VM scope:
            // busy flips before processing and navigating away no
            // longer silently discards the pick.
            viewModel.publishAvatar { processor.process(uri) }
        }
    }
    ProfileContent(
        state = state,
        identity = ProfileIdentity(name = selfName, avatar = avatar, restAvatarUrl = restAvatarUrl),
        actions = ProfileActions(
            onBack = onBack,
            onPickAvatar = {
                pickAvatar.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly))
            },
            onRemoveAvatar = viewModel::removeAvatar,
            onFullName = viewModel::setFullName,
            onNickname = viewModel::setNickname,
            onPronouns = viewModel::setPronouns,
            onUrl = viewModel::setUrl,
            onNote = viewModel::setNote,
            onSaveVcard = viewModel::saveVcard,
            onRevertVcard = viewModel::revertVcard,
            onRetryLoad = viewModel::retryLoad,
            onMoodKind = viewModel::setMoodKind,
            onMoodText = viewModel::setMoodText,
            onPublishMood = viewModel::publishMood,
            onClearMood = viewModel::clearMood,
            onActivityGeneral = viewModel::setActivityGeneral,
            onActivitySpecific = viewModel::setActivitySpecific,
            onActivityText = viewModel::setActivityText,
            onPublishActivity = viewModel::publishActivity,
            onClearActivity = viewModel::clearActivity,
            onTuneField = viewModel::setTuneField,
            onPublishTune = viewModel::publishTune,
            onClearTune = viewModel::clearTune,
        ),
    )
}

/** Whose profile this is: the picture and the name it falls back to. */
internal data class ProfileIdentity(
    val name: String,
    val avatar: WaddleAvatar? = null,
    val restAvatarUrl: String? = null,
)

/** Everything the profile surface can ask for. */
internal data class ProfileActions(
    val onBack: () -> Unit = {},
    val onPickAvatar: () -> Unit = {},
    val onRemoveAvatar: () -> Unit = {},
    val onFullName: (String) -> Unit = {},
    val onNickname: (String) -> Unit = {},
    val onPronouns: (String) -> Unit = {},
    val onUrl: (String) -> Unit = {},
    val onNote: (String) -> Unit = {},
    val onSaveVcard: () -> Unit = {},
    val onRevertVcard: () -> Unit = {},
    val onRetryLoad: () -> Unit = {},
    val onMoodKind: (String) -> Unit = {},
    val onMoodText: (String) -> Unit = {},
    val onPublishMood: () -> Unit = {},
    val onClearMood: () -> Unit = {},
    val onActivityGeneral: (String) -> Unit = {},
    val onActivitySpecific: (String) -> Unit = {},
    val onActivityText: (String) -> Unit = {},
    val onPublishActivity: () -> Unit = {},
    val onClearActivity: () -> Unit = {},
    val onTuneField: (ProfileViewModel.TuneField, String) -> Unit = { _, _ -> },
    val onPublishTune: () -> Unit = {},
    val onClearTune: () -> Unit = {},
)

/** Stateless profile editor: who you are, then one card per thing you publish. */
@Composable
internal fun ProfileContent(state: ProfileUiState, identity: ProfileIdentity, actions: ProfileActions) {
    Scaffold(
        containerColor = Waddle.colors.bg,
        topBar = {
            WaddleTopBar(
                title = { Text(text = stringResource(R.string.profile_title)) },
                navigationIcon = {
                    IconButton(onClick = actions.onBack) {
                        Icon(
                            Icons.AutoMirrored.Outlined.ArrowBack,
                            contentDescription = stringResource(R.string.action_back),
                        )
                    }
                },
            )
        },
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .consumeWindowInsets(padding)
                // The keyboard shrinks the scroll viewport, so a focused
                // field low in the form scrolls into view above it.
                .imePadding()
                .verticalScroll(rememberScrollState())
                .padding(padding)
                .padding(bottom = Waddle.space.xl),
        ) {
            AvatarSection(state = state.avatar, identity = identity, actions = actions)
            ProfileCard(title = stringResource(R.string.huddle_profile_about)) {
                VcardSection(state = state.vcard, actions = actions)
            }
            MoodCard(state = state.mood, actions = actions)
            ProfileCard(title = stringResource(R.string.huddle_profile_activity)) {
                ActivitySection(state = state.activity, actions = actions)
            }
            ProfileCard(title = stringResource(R.string.huddle_profile_tune)) {
                TuneSection(state = state.tune, actions = actions)
            }
        }
    }
}

// ── Avatar (XEP-0084) ─────────────────────────────────────────────────

@Composable
private fun AvatarSection(state: AvatarSectionState, identity: ProfileIdentity, actions: ProfileActions) {
    // Identity first: the picture large and centred, its two actions under it.
    Column(
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(Waddle.space.sm),
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = Waddle.space.md, vertical = Waddle.space.md),
    ) {
        SelfAvatarImage(avatar = identity.avatar, restAvatarUrl = identity.restAvatarUrl, selfName = identity.name)
        Text(
            text = identity.name,
            style = MaterialTheme.typography.headlineSmall,
            color = Waddle.colors.fg,
            modifier = Modifier.semantics { heading() },
        )
        Row(
            horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            PillButton(
                text = stringResource(R.string.profile_avatar_change),
                onClick = actions.onPickAvatar,
                enabled = !state.busy,
                tone = PillTone.QUIET,
                icon = Icons.Outlined.PhotoCamera,
                modifier = Modifier.testTag(ProfileScreenTestTags.AVATAR_CHANGE),
            )
            if (identity.avatar != null) {
                TextButton(
                    onClick = actions.onRemoveAvatar,
                    enabled = !state.busy,
                    modifier = Modifier.testTag(ProfileScreenTestTags.AVATAR_REMOVE),
                ) {
                    Text(text = stringResource(R.string.profile_avatar_remove), color = Waddle.colors.dangerText)
                }
            }
        }
        FeedbackText(feedback = state.feedback)
    }
}

/** A profile section: kicker plus a padded card of fields. */
@Composable
private fun ProfileCard(title: String, content: @Composable ColumnScope.() -> Unit) {
    GroupedSection(title = title) {
        Column(
            verticalArrangement = Arrangement.spacedBy(Waddle.space.sm),
            modifier = Modifier.padding(Waddle.space.md),
            content = content,
        )
    }
}

/** XMPP-published bytes win; the REST avatar URL is the fallback. */
@Composable
private fun SelfAvatarImage(avatar: WaddleAvatar?, restAvatarUrl: String?, selfName: String) {
    val avatarLabel = stringResource(R.string.settings_avatar)
    val modifier = Modifier
        .size(PROFILE_AVATAR_SIZE)
        .clip(CircleShape)
    val bitmap = remember(avatar?.id) {
        avatar?.data?.let { bytes ->
            BitmapFactory.decodeByteArray(bytes, 0, bytes.size)?.asImageBitmap()
        }
    }
    when {
        bitmap != null -> Image(
            bitmap = bitmap,
            contentDescription = avatarLabel,
            contentScale = ContentScale.Crop,
            modifier = modifier.testTag(ProfileScreenTestTags.AVATAR_XMPP_IMAGE),
        )
        restAvatarUrl != null -> AsyncImage(
            model = restAvatarUrl,
            contentDescription = avatarLabel,
            contentScale = ContentScale.Crop,
            modifier = modifier,
        )
        // No image yet: initials, like every other person surface.
        else -> PeerAvatar(
            jid = null,
            displayName = selfName,
            size = PROFILE_AVATAR_SIZE,
            modifier = Modifier.semantics { contentDescription = avatarLabel },
        )
    }
}

// ── vCard4 (XEP-0292) ─────────────────────────────────────────────────

@Composable
private fun VcardSection(state: VcardSectionState, actions: ProfileActions) {
    val enabled = !state.loading && !state.saving
    ProfileField(
        value = state.fullName,
        onValueChange = actions.onFullName,
        labelRes = R.string.profile_full_name,
        enabled = enabled,
        testTag = ProfileScreenTestTags.VCARD_FULL_NAME,
    )
    FieldPair {
        ProfileField(
            value = state.nickname,
            onValueChange = actions.onNickname,
            labelRes = R.string.profile_nickname,
            enabled = enabled,
            testTag = ProfileScreenTestTags.VCARD_NICKNAME,
            modifier = Modifier.weight(1f),
        )
        ProfileField(
            value = state.pronouns,
            onValueChange = actions.onPronouns,
            labelRes = R.string.profile_pronouns,
            enabled = enabled,
            testTag = ProfileScreenTestTags.VCARD_PRONOUNS,
            modifier = Modifier.weight(1f),
        )
    }
    ProfileField(
        value = state.url,
        onValueChange = actions.onUrl,
        labelRes = R.string.profile_url,
        enabled = enabled,
        testTag = ProfileScreenTestTags.VCARD_URL,
    )
    ProfileField(
        value = state.note,
        onValueChange = actions.onNote,
        labelRes = R.string.profile_note,
        enabled = enabled,
        testTag = ProfileScreenTestTags.VCARD_NOTE,
        singleLine = false,
    )
    if (state.loadFailed) {
        Row(
            horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                text = stringResource(R.string.profile_load_failed),
                style = Waddle.type.control,
                color = Waddle.colors.dangerText,
                modifier = Modifier.weight(1f),
            )
            TextButton(
                onClick = actions.onRetryLoad,
                modifier = Modifier.testTag(ProfileScreenTestTags.VCARD_RETRY),
            ) {
                Text(text = stringResource(R.string.profile_retry), color = Waddle.colors.actionText)
            }
        }
    }
    SectionFooter(
        feedback = state.feedback,
        feedbackTag = ProfileScreenTestTags.VCARD_FEEDBACK,
        primary = FooterButton(
            text = stringResource(if (state.saving) R.string.profile_saving else R.string.profile_save),
            onClick = actions.onSaveVcard,
            enabled = enabled && state.loaded,
            testTag = ProfileScreenTestTags.VCARD_SAVE,
        ),
        secondary = FooterButton(
            text = stringResource(R.string.profile_revert),
            onClick = actions.onRevertVcard,
            enabled = enabled,
            testTag = ProfileScreenTestTags.VCARD_REVERT,
        ),
    )
}

// ── Mood (XEP-0107) ───────────────────────────────────────────────────

@Composable
private fun MoodCard(state: MoodSectionState, actions: ProfileActions) {
    var pickerOpen by remember { mutableStateOf(false) }
    GroupedSection(title = stringResource(R.string.profile_mood_section)) {
        GroupedRow(
            title = if (state.draft.kind.isEmpty()) {
                stringResource(R.string.profile_mood_choose)
            } else {
                formatPepKeyword(state.draft.kind)
            },
            titleColor = if (state.draft.kind.isEmpty()) Waddle.colors.fgMuted else Waddle.colors.fg,
            leading = { GlyphTile(Icons.Outlined.Mood) },
            trailing = { RowChevron() },
            onClick = { if (!state.busy) pickerOpen = true },
            modifier = Modifier.testTag(ProfileScreenTestTags.MOOD_PICKER),
        )
        Column(
            verticalArrangement = Arrangement.spacedBy(Waddle.space.sm),
            modifier = Modifier.padding(start = Waddle.space.md, end = Waddle.space.md, bottom = Waddle.space.md),
        ) {
            ProfileField(
                value = state.draft.text,
                onValueChange = actions.onMoodText,
                labelRes = R.string.profile_optional_note,
                enabled = !state.busy,
                testTag = ProfileScreenTestTags.MOOD_TEXT,
            )
            PublishClearFooter(
                busy = state.busy,
                feedback = state.feedback,
                onPublish = actions.onPublishMood,
                onClear = actions.onClearMood,
                publishTag = ProfileScreenTestTags.MOOD_PUBLISH,
                clearTag = ProfileScreenTestTags.MOOD_CLEAR,
            )
        }
    }
    if (pickerOpen) {
        MoodPickerSheet(
            selected = state.draft.kind,
            onSelect = { kind ->
                actions.onMoodKind(kind)
                pickerOpen = false
            },
            onDismiss = { pickerOpen = false },
        )
    }
}

/** Searchable picker over the 84 XEP-0107 mood kinds, as proper rows. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun MoodPickerSheet(selected: String, onSelect: (String) -> Unit, onDismiss: () -> Unit) {
    var query by remember { mutableStateOf("") }
    val colors = Waddle.colors
    val matches = MOOD_KINDS.filter { kind ->
        query.isBlank() || formatPepKeyword(kind).contains(query.trim(), ignoreCase = true)
    }
    ModalBottomSheet(onDismissRequest = onDismiss, containerColor = colors.surface) {
        Column(modifier = Modifier.navigationBarsPadding().imePadding()) {
            SheetTitle(text = stringResource(R.string.profile_mood_choose))
            OutlinedTextField(
                value = query,
                onValueChange = { query = it },
                placeholder = { Text(text = stringResource(R.string.profile_mood_search)) },
                leadingIcon = { Icon(Icons.Outlined.Search, contentDescription = null) },
                singleLine = true,
                shape = HuddleFieldShape,
                colors = huddleFieldColors(),
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = Waddle.space.md)
                    .testTag(ProfileScreenTestTags.MOOD_SEARCH),
            )
            LazyColumn(modifier = Modifier.padding(top = Waddle.space.xs)) {
                items(matches, key = { it }) { kind ->
                    val chosen = kind == selected
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        modifier = Modifier
                            .fillMaxWidth()
                            .clickable { onSelect(kind) }
                            .heightIn(min = PICKER_ROW)
                            .padding(horizontal = Waddle.space.md)
                            .testTag("profile-mood-option-$kind"),
                    ) {
                        Text(
                            text = formatPepKeyword(kind),
                            style = Waddle.type.body,
                            color = if (chosen) colors.actionText else colors.fg,
                            modifier = Modifier.weight(1f),
                        )
                        if (chosen) {
                            Icon(Icons.Outlined.Check, contentDescription = null, tint = colors.actionText)
                        }
                    }
                }
            }
        }
    }
}

// ── Activity (XEP-0108) ───────────────────────────────────────────────

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ActivitySection(state: ActivitySectionState, actions: ProfileActions) {
    var menuOpen by remember { mutableStateOf(false) }
    ExposedDropdownMenuBox(expanded = menuOpen, onExpandedChange = { menuOpen = it }) {
        OutlinedTextField(
            value = if (state.draft.general.isEmpty()) "" else formatPepKeyword(state.draft.general),
            onValueChange = {},
            readOnly = true,
            label = { Text(text = stringResource(R.string.profile_activity_general)) },
            trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = menuOpen) },
            isError = ActivityFieldError.MISSING_GENERAL in state.errors,
            shape = HuddleFieldShape,
            colors = huddleFieldColors(),
            modifier = Modifier
                .fillMaxWidth()
                .menuAnchor(ExposedDropdownMenuAnchorType.PrimaryNotEditable)
                .testTag(ProfileScreenTestTags.ACTIVITY_GENERAL),
        )
        ExposedDropdownMenu(
            expanded = menuOpen,
            onDismissRequest = { menuOpen = false },
            containerColor = Waddle.colors.surface,
        ) {
            GENERAL_ACTIVITIES.forEach { general ->
                DropdownMenuItem(
                    text = { Text(text = formatPepKeyword(general)) },
                    onClick = {
                        actions.onActivityGeneral(general)
                        menuOpen = false
                    },
                    modifier = Modifier.testTag("profile-activity-option-$general"),
                )
            }
        }
    }
    ProfileField(
        value = state.draft.specific,
        onValueChange = actions.onActivitySpecific,
        labelRes = R.string.profile_activity_specific,
        enabled = !state.busy,
        testTag = ProfileScreenTestTags.ACTIVITY_SPECIFIC,
        isError = ActivityFieldError.INVALID_SPECIFIC in state.errors,
    )
    ProfileField(
        value = state.draft.text,
        onValueChange = actions.onActivityText,
        labelRes = R.string.profile_optional_note,
        enabled = !state.busy,
        testTag = ProfileScreenTestTags.ACTIVITY_TEXT,
    )
    PublishClearFooter(
        busy = state.busy,
        feedback = state.feedback,
        onPublish = actions.onPublishActivity,
        onClear = actions.onClearActivity,
        publishTag = ProfileScreenTestTags.ACTIVITY_PUBLISH,
        clearTag = ProfileScreenTestTags.ACTIVITY_CLEAR,
    )
}

// ── Tune (XEP-0118) ───────────────────────────────────────────────────

@Composable
private fun TuneSection(state: TuneSectionState, actions: ProfileActions) {
    TuneDraftField(state, actions, ProfileViewModel.TuneField.TITLE, R.string.profile_tune_title, state.draft.title)
    TuneDraftField(state, actions, ProfileViewModel.TuneField.ARTIST, R.string.profile_tune_artist, state.draft.artist)
    FieldPair {
        TuneDraftField(
            state,
            actions,
            ProfileViewModel.TuneField.SOURCE,
            R.string.profile_tune_source,
            state.draft.source,
            modifier = Modifier.weight(1f),
        )
        TuneDraftField(
            state,
            actions,
            ProfileViewModel.TuneField.TRACK,
            R.string.profile_tune_track,
            state.draft.track,
            modifier = Modifier.weight(1f),
        )
    }
    FieldPair {
        TuneDraftField(
            state,
            actions,
            ProfileViewModel.TuneField.LENGTH,
            R.string.profile_tune_length,
            state.draft.length,
            isError = TuneFieldError.INVALID_LENGTH in state.errors,
            modifier = Modifier.weight(1f),
        )
        TuneDraftField(
            state,
            actions,
            ProfileViewModel.TuneField.RATING,
            R.string.profile_tune_rating,
            state.draft.rating,
            isError = TuneFieldError.INVALID_RATING in state.errors,
            modifier = Modifier.weight(1f),
        )
    }
    TuneDraftField(
        state,
        actions,
        ProfileViewModel.TuneField.URI,
        R.string.profile_tune_uri,
        state.draft.uri,
        isError = TuneFieldError.INVALID_URI in state.errors,
    )
    if (TuneFieldError.EMPTY_FORM in state.errors) {
        Text(
            text = stringResource(R.string.profile_tune_empty),
            style = Waddle.type.control,
            color = Waddle.colors.dangerText,
        )
    }
    PublishClearFooter(
        busy = state.busy,
        feedback = state.feedback,
        onPublish = actions.onPublishTune,
        onClear = actions.onClearTune,
        publishTag = ProfileScreenTestTags.TUNE_PUBLISH,
        clearTag = ProfileScreenTestTags.TUNE_CLEAR,
    )
}

@Composable
private fun TuneDraftField(
    state: TuneSectionState,
    actions: ProfileActions,
    field: ProfileViewModel.TuneField,
    labelRes: Int,
    value: String,
    modifier: Modifier = Modifier,
    isError: Boolean = false,
) {
    ProfileField(
        value = value,
        onValueChange = { actions.onTuneField(field, it) },
        labelRes = labelRes,
        enabled = !state.busy,
        testTag = "profile-tune-${field.name.lowercase()}",
        isError = isError,
        modifier = modifier,
    )
}

// ── Shared pieces ─────────────────────────────────────────────────────

/** Two short fields side by side. */
@Composable
private fun FieldPair(content: @Composable RowScope.() -> Unit) {
    Row(horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm), content = content)
}

@Composable
private fun ProfileField(
    value: String,
    onValueChange: (String) -> Unit,
    labelRes: Int,
    enabled: Boolean,
    testTag: String,
    modifier: Modifier = Modifier,
    singleLine: Boolean = true,
    isError: Boolean = false,
) {
    OutlinedTextField(
        value = value,
        onValueChange = onValueChange,
        label = { Text(text = stringResource(labelRes), maxLines = 1) },
        enabled = enabled,
        singleLine = singleLine,
        minLines = if (singleLine) 1 else NOTE_LINES,
        isError = isError,
        shape = HuddleFieldShape,
        colors = huddleFieldColors(),
        // Full width of whatever slot it is given: the whole form, or a
        // weighted half of a FieldPair.
        modifier = modifier.fillMaxWidth().testTag(testTag),
    )
}

/** One button of a section footer. */
private data class FooterButton(
    val text: String,
    val onClick: () -> Unit,
    val enabled: Boolean,
    val testTag: String,
)

/**
 * The end of a section: what happened last (left), then the quiet
 * secondary and the teal primary (right). One button style everywhere.
 */
@Composable
private fun SectionFooter(
    feedback: ProfileFeedback?,
    primary: FooterButton,
    secondary: FooterButton,
    feedbackTag: String? = null,
) {
    Row(
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs),
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier.padding(top = Waddle.space.xxs),
    ) {
        Column(modifier = Modifier.weight(1f)) {
            FeedbackText(feedback = feedback, testTag = feedbackTag)
        }
        PillButton(
            text = secondary.text,
            onClick = secondary.onClick,
            enabled = secondary.enabled,
            tone = PillTone.QUIET,
            modifier = Modifier.testTag(secondary.testTag),
        )
        PillButton(
            text = primary.text,
            onClick = primary.onClick,
            enabled = primary.enabled,
            modifier = Modifier.testTag(primary.testTag),
        )
    }
}

@Composable
private fun PublishClearFooter(
    busy: Boolean,
    feedback: ProfileFeedback?,
    onPublish: () -> Unit,
    onClear: () -> Unit,
    publishTag: String,
    clearTag: String,
) {
    SectionFooter(
        feedback = feedback,
        primary = FooterButton(stringResource(R.string.profile_publish), onPublish, !busy, publishTag),
        secondary = FooterButton(stringResource(R.string.profile_clear), onClear, !busy, clearTag),
    )
}

@Composable
private fun FeedbackText(feedback: ProfileFeedback?, testTag: String? = null) {
    val message = feedback?.let { stringResource(it.messageRes()) } ?: return
    val isError = feedback == ProfileFeedback.FAILED ||
        feedback == ProfileFeedback.OFFLINE ||
        feedback == ProfileFeedback.INVALID
    Text(
        text = message,
        style = Waddle.type.control.copy(fontWeight = null),
        color = if (isError) Waddle.colors.dangerText else Waddle.colors.fgMuted,
        modifier = testTag?.let { Modifier.testTag(it) } ?: Modifier,
    )
}

private fun ProfileFeedback.messageRes(): Int = when (this) {
    ProfileFeedback.PUBLISHED -> R.string.profile_feedback_published
    ProfileFeedback.CLEARED -> R.string.profile_feedback_cleared
    ProfileFeedback.UNCHANGED -> R.string.profile_feedback_unchanged
    ProfileFeedback.INVALID -> R.string.profile_feedback_invalid
    ProfileFeedback.FAILED -> R.string.profile_feedback_failed
    ProfileFeedback.OFFLINE -> R.string.profile_feedback_offline
}

private const val NOTE_LINES = 3
private val PROFILE_AVATAR_SIZE = 96.dp
private val PICKER_ROW = 48.dp

/** Semantics tags shared with instrumented tests. */
object ProfileScreenTestTags {
    const val AVATAR_CHANGE = "profile-avatar-change"
    const val AVATAR_REMOVE = "profile-avatar-remove"
    const val AVATAR_XMPP_IMAGE = "profile-avatar-xmpp-image"
    const val VCARD_FULL_NAME = "profile-vcard-full-name"
    const val VCARD_NICKNAME = "profile-vcard-nickname"
    const val VCARD_PRONOUNS = "profile-vcard-pronouns"
    const val VCARD_URL = "profile-vcard-url"
    const val VCARD_NOTE = "profile-vcard-note"
    const val VCARD_SAVE = "profile-vcard-save"
    const val VCARD_REVERT = "profile-vcard-revert"
    const val VCARD_RETRY = "profile-vcard-retry"
    const val VCARD_FEEDBACK = "profile-vcard-feedback"
    const val MOOD_PICKER = "profile-mood-picker"
    const val MOOD_SEARCH = "profile-mood-search"
    const val MOOD_TEXT = "profile-mood-text"
    const val MOOD_PUBLISH = "profile-mood-publish"
    const val MOOD_CLEAR = "profile-mood-clear"
    const val ACTIVITY_GENERAL = "profile-activity-general"
    const val ACTIVITY_SPECIFIC = "profile-activity-specific"
    const val ACTIVITY_TEXT = "profile-activity-text"
    const val ACTIVITY_PUBLISH = "profile-activity-publish"
    const val ACTIVITY_CLEAR = "profile-activity-clear"
    const val TUNE_PUBLISH = "profile-tune-publish"
    const val TUNE_CLEAR = "profile-tune-clear"
}

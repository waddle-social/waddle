package social.waddle.android.feature.profile

import android.graphics.BitmapFactory
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.Image
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.outlined.AccountCircle
import androidx.compose.material3.Button
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExposedDropdownMenuAnchorType
import androidx.compose.material3.ExposedDropdownMenuBox
import androidx.compose.material3.ExposedDropdownMenuDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
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
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import coil3.compose.AsyncImage
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.client.GENERAL_ACTIVITIES
import social.waddle.android.client.MOOD_KINDS
import social.waddle.client.ffi.WaddleAvatar

/**
 * Own-profile publishing (web `UserSettingsPage` + `VCardEditor`
 * parity): XEP-0084 avatar, XEP-0292 vCard4 editor with optimistic
 * save, and the XEP-0107/0108/0118 status signal sections.
 */
@OptIn(ExperimentalMaterial3Api::class)
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

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(text = stringResource(R.string.profile_title)) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(
                            Icons.AutoMirrored.Filled.ArrowBack,
                            contentDescription = stringResource(R.string.action_back),
                        )
                    }
                },
            )
        },
    ) { padding ->
        Column(
            modifier = Modifier
                .padding(padding)
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            AvatarSection(state = state.avatar, avatar = avatar, viewModel = viewModel)
            SectionHeader(text = stringResource(R.string.profile_vcard_section))
            VcardSection(state = state.vcard, viewModel = viewModel)
            SectionHeader(text = stringResource(R.string.profile_mood_section))
            MoodSection(state = state.mood, viewModel = viewModel)
            SectionHeader(text = stringResource(R.string.profile_activity_section))
            ActivitySection(state = state.activity, viewModel = viewModel)
            SectionHeader(text = stringResource(R.string.profile_tune_section))
            TuneSection(state = state.tune, viewModel = viewModel)
        }
    }
}

// ── Avatar (XEP-0084) ─────────────────────────────────────────────────

@Composable
private fun AvatarSection(
    state: AvatarSectionState,
    avatar: WaddleAvatar?,
    viewModel: ProfileViewModel,
) {
    val context = LocalContext.current
    val processor = remember { BitmapAvatarProcessor(context.contentResolver) }
    val restAvatarUrl by viewModel.restAvatarUrl.collectAsStateWithLifecycle()
    val pickAvatar = rememberLauncherForActivityResult(
        ActivityResultContracts.PickVisualMedia(),
    ) { uri ->
        if (uri != null) {
            // The whole process→publish pipeline runs in the VM scope:
            // busy flips before processing and navigating away no
            // longer silently discards the pick.
            viewModel.publishAvatar { processor.process(uri) }
        }
    }

    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(top = 16.dp),
        horizontalArrangement = Arrangement.spacedBy(16.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        SelfAvatarImage(avatar = avatar, restAvatarUrl = restAvatarUrl)
        Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedButton(
                    onClick = {
                        pickAvatar.launch(
                            PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly),
                        )
                    },
                    enabled = !state.busy,
                    modifier = Modifier.testTag(ProfileScreenTestTags.AVATAR_CHANGE),
                ) {
                    Text(text = stringResource(R.string.profile_avatar_change))
                }
                OutlinedButton(
                    onClick = viewModel::removeAvatar,
                    enabled = !state.busy && avatar != null,
                    modifier = Modifier.testTag(ProfileScreenTestTags.AVATAR_REMOVE),
                ) {
                    Text(text = stringResource(R.string.profile_avatar_remove))
                }
            }
            FeedbackText(feedback = state.feedback)
        }
    }
}

/** XMPP-published bytes win; the REST avatar URL is the fallback. */
@Composable
private fun SelfAvatarImage(avatar: WaddleAvatar?, restAvatarUrl: String?) {
    val modifier = Modifier
        .size(64.dp)
        .clip(CircleShape)
    val bitmap = remember(avatar?.id) {
        avatar?.data?.let { bytes ->
            BitmapFactory.decodeByteArray(bytes, 0, bytes.size)?.asImageBitmap()
        }
    }
    when {
        bitmap != null -> Image(
            bitmap = bitmap,
            contentDescription = stringResource(R.string.settings_avatar),
            contentScale = ContentScale.Crop,
            modifier = modifier.testTag(ProfileScreenTestTags.AVATAR_XMPP_IMAGE),
        )
        restAvatarUrl != null -> AsyncImage(
            model = restAvatarUrl,
            contentDescription = stringResource(R.string.settings_avatar),
            contentScale = ContentScale.Crop,
            modifier = modifier,
        )
        else -> Icon(
            Icons.Outlined.AccountCircle,
            contentDescription = stringResource(R.string.settings_avatar),
            modifier = Modifier.size(64.dp),
        )
    }
}

// ── vCard4 (XEP-0292) ─────────────────────────────────────────────────

@Composable
private fun VcardSection(state: VcardSectionState, viewModel: ProfileViewModel) {
    val enabled = !state.loading && !state.saving
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        ProfileField(
            value = state.fullName,
            onValueChange = viewModel::setFullName,
            labelRes = R.string.profile_full_name,
            enabled = enabled,
            testTag = ProfileScreenTestTags.VCARD_FULL_NAME,
        )
        ProfileField(
            value = state.nickname,
            onValueChange = viewModel::setNickname,
            labelRes = R.string.profile_nickname,
            enabled = enabled,
            testTag = ProfileScreenTestTags.VCARD_NICKNAME,
        )
        ProfileField(
            value = state.pronouns,
            onValueChange = viewModel::setPronouns,
            labelRes = R.string.profile_pronouns,
            enabled = enabled,
            testTag = ProfileScreenTestTags.VCARD_PRONOUNS,
        )
        ProfileField(
            value = state.url,
            onValueChange = viewModel::setUrl,
            labelRes = R.string.profile_url,
            enabled = enabled,
            testTag = ProfileScreenTestTags.VCARD_URL,
        )
        ProfileField(
            value = state.note,
            onValueChange = viewModel::setNote,
            labelRes = R.string.profile_note,
            enabled = enabled,
            testTag = ProfileScreenTestTags.VCARD_NOTE,
            singleLine = false,
        )
        Row(
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Button(
                onClick = viewModel::saveVcard,
                enabled = enabled && state.loaded,
                modifier = Modifier.testTag(ProfileScreenTestTags.VCARD_SAVE),
            ) {
                Text(
                    text = stringResource(
                        if (state.saving) R.string.profile_saving else R.string.profile_save,
                    ),
                )
            }
            OutlinedButton(
                onClick = viewModel::revertVcard,
                enabled = enabled,
                modifier = Modifier.testTag(ProfileScreenTestTags.VCARD_REVERT),
            ) {
                Text(text = stringResource(R.string.profile_revert))
            }
        }
        if (state.loadFailed) {
            Row(
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    text = stringResource(R.string.profile_load_failed),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                )
                OutlinedButton(
                    onClick = viewModel::retryLoad,
                    modifier = Modifier.testTag(ProfileScreenTestTags.VCARD_RETRY),
                ) {
                    Text(text = stringResource(R.string.profile_retry))
                }
            }
        }
        FeedbackText(feedback = state.feedback, testTag = ProfileScreenTestTags.VCARD_FEEDBACK)
    }
}

// ── Mood (XEP-0107) ───────────────────────────────────────────────────

@Composable
private fun MoodSection(state: MoodSectionState, viewModel: ProfileViewModel) {
    var pickerOpen by remember { mutableStateOf(false) }
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        val pickerLabel = if (state.draft.kind.isEmpty()) {
            stringResource(R.string.profile_mood_choose)
        } else {
            formatPepKeyword(state.draft.kind)
        }
        OutlinedButton(
            onClick = { pickerOpen = true },
            enabled = !state.busy,
            modifier = Modifier.testTag(ProfileScreenTestTags.MOOD_PICKER),
        ) {
            Text(text = pickerLabel)
        }
        ProfileField(
            value = state.draft.text,
            onValueChange = viewModel::setMoodText,
            labelRes = R.string.profile_optional_note,
            enabled = !state.busy,
            testTag = ProfileScreenTestTags.MOOD_TEXT,
        )
        PublishClearRow(
            busy = state.busy,
            feedback = state.feedback,
            onPublish = viewModel::publishMood,
            onClear = viewModel::clearMood,
            publishTag = ProfileScreenTestTags.MOOD_PUBLISH,
            clearTag = ProfileScreenTestTags.MOOD_CLEAR,
        )
    }
    if (pickerOpen) {
        MoodPickerDialog(
            onSelect = { kind ->
                viewModel.setMoodKind(kind)
                pickerOpen = false
            },
            onDismiss = { pickerOpen = false },
        )
    }
}

/** Searchable picker over the 84 XEP-0107 mood kinds. */
@Composable
private fun MoodPickerDialog(onSelect: (String) -> Unit, onDismiss: () -> Unit) {
    var query by remember { mutableStateOf("") }
    val matches = MOOD_KINDS.filter { kind ->
        query.isBlank() || formatPepKeyword(kind).contains(query.trim(), ignoreCase = true)
    }
    Dialog(onDismissRequest = onDismiss) {
        Surface(
            shape = MaterialTheme.shapes.large,
            modifier = Modifier.heightIn(max = 480.dp),
        ) {
            Column(modifier = Modifier.padding(16.dp)) {
                OutlinedTextField(
                    value = query,
                    onValueChange = { query = it },
                    label = { Text(text = stringResource(R.string.profile_mood_search)) },
                    singleLine = true,
                    modifier = Modifier
                        .fillMaxWidth()
                        .testTag(ProfileScreenTestTags.MOOD_SEARCH),
                )
                LazyColumn(modifier = Modifier.padding(top = 8.dp)) {
                    items(matches) { kind ->
                        Text(
                            text = formatPepKeyword(kind),
                            style = MaterialTheme.typography.bodyLarge,
                            modifier = Modifier
                                .fillMaxWidth()
                                .clickable { onSelect(kind) }
                                .padding(vertical = 12.dp)
                                .testTag("profile-mood-option-$kind"),
                        )
                    }
                }
            }
        }
    }
}

// ── Activity (XEP-0108) ───────────────────────────────────────────────

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ActivitySection(state: ActivitySectionState, viewModel: ProfileViewModel) {
    var menuOpen by remember { mutableStateOf(false) }
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        ExposedDropdownMenuBox(expanded = menuOpen, onExpandedChange = { menuOpen = it }) {
            OutlinedTextField(
                value = if (state.draft.general.isEmpty()) "" else formatPepKeyword(state.draft.general),
                onValueChange = {},
                readOnly = true,
                label = { Text(text = stringResource(R.string.profile_activity_general)) },
                trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = menuOpen) },
                isError = ActivityFieldError.MISSING_GENERAL in state.errors,
                modifier = Modifier
                    .fillMaxWidth()
                    .menuAnchor(ExposedDropdownMenuAnchorType.PrimaryNotEditable)
                    .testTag(ProfileScreenTestTags.ACTIVITY_GENERAL),
            )
            ExposedDropdownMenu(expanded = menuOpen, onDismissRequest = { menuOpen = false }) {
                GENERAL_ACTIVITIES.forEach { general ->
                    DropdownMenuItem(
                        text = { Text(text = formatPepKeyword(general)) },
                        onClick = {
                            viewModel.setActivityGeneral(general)
                            menuOpen = false
                        },
                        modifier = Modifier.testTag("profile-activity-option-$general"),
                    )
                }
            }
        }
        ProfileField(
            value = state.draft.specific,
            onValueChange = viewModel::setActivitySpecific,
            labelRes = R.string.profile_activity_specific,
            enabled = !state.busy,
            testTag = ProfileScreenTestTags.ACTIVITY_SPECIFIC,
            isError = ActivityFieldError.INVALID_SPECIFIC in state.errors,
        )
        ProfileField(
            value = state.draft.text,
            onValueChange = viewModel::setActivityText,
            labelRes = R.string.profile_optional_note,
            enabled = !state.busy,
            testTag = ProfileScreenTestTags.ACTIVITY_TEXT,
        )
        PublishClearRow(
            busy = state.busy,
            feedback = state.feedback,
            onPublish = viewModel::publishActivity,
            onClear = viewModel::clearActivity,
            publishTag = ProfileScreenTestTags.ACTIVITY_PUBLISH,
            clearTag = ProfileScreenTestTags.ACTIVITY_CLEAR,
        )
    }
}

// ── Tune (XEP-0118) ───────────────────────────────────────────────────

@Composable
private fun TuneSection(state: TuneSectionState, viewModel: ProfileViewModel) {
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        TuneDraftField(
            state,
            viewModel,
            ProfileViewModel.TuneField.TITLE,
            R.string.profile_tune_title,
            state.draft.title,
        )
        TuneDraftField(
            state,
            viewModel,
            ProfileViewModel.TuneField.ARTIST,
            R.string.profile_tune_artist,
            state.draft.artist,
        )
        TuneDraftField(
            state,
            viewModel,
            ProfileViewModel.TuneField.SOURCE,
            R.string.profile_tune_source,
            state.draft.source,
        )
        TuneDraftField(
            state,
            viewModel,
            ProfileViewModel.TuneField.TRACK,
            R.string.profile_tune_track,
            state.draft.track,
        )
        TuneDraftField(
            state,
            viewModel,
            ProfileViewModel.TuneField.LENGTH,
            R.string.profile_tune_length,
            state.draft.length,
            isError = TuneFieldError.INVALID_LENGTH in state.errors,
        )
        TuneDraftField(
            state,
            viewModel,
            ProfileViewModel.TuneField.RATING,
            R.string.profile_tune_rating,
            state.draft.rating,
            isError = TuneFieldError.INVALID_RATING in state.errors,
        )
        TuneDraftField(
            state,
            viewModel,
            ProfileViewModel.TuneField.URI,
            R.string.profile_tune_uri,
            state.draft.uri,
            isError = TuneFieldError.INVALID_URI in state.errors,
        )
        if (TuneFieldError.EMPTY_FORM in state.errors) {
            Text(
                text = stringResource(R.string.profile_tune_empty),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.error,
            )
        }
        PublishClearRow(
            busy = state.busy,
            feedback = state.feedback,
            onPublish = viewModel::publishTune,
            onClear = viewModel::clearTune,
            publishTag = ProfileScreenTestTags.TUNE_PUBLISH,
            clearTag = ProfileScreenTestTags.TUNE_CLEAR,
        )
    }
}

@Composable
private fun TuneDraftField(
    state: TuneSectionState,
    viewModel: ProfileViewModel,
    field: ProfileViewModel.TuneField,
    labelRes: Int,
    value: String,
    isError: Boolean = false,
) {
    ProfileField(
        value = value,
        onValueChange = { viewModel.setTuneField(field, it) },
        labelRes = labelRes,
        enabled = !state.busy,
        testTag = "profile-tune-${field.name.lowercase()}",
        isError = isError,
    )
}

// ── Shared pieces ─────────────────────────────────────────────────────

@Composable
private fun ProfileField(
    value: String,
    onValueChange: (String) -> Unit,
    labelRes: Int,
    enabled: Boolean,
    testTag: String,
    singleLine: Boolean = true,
    isError: Boolean = false,
) {
    OutlinedTextField(
        value = value,
        onValueChange = onValueChange,
        label = { Text(text = stringResource(labelRes)) },
        enabled = enabled,
        singleLine = singleLine,
        isError = isError,
        modifier = Modifier
            .fillMaxWidth()
            .testTag(testTag),
    )
}

@Composable
private fun PublishClearRow(
    busy: Boolean,
    feedback: ProfileFeedback?,
    onPublish: () -> Unit,
    onClear: () -> Unit,
    publishTag: String,
    clearTag: String,
) {
    Row(
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Button(
            onClick = onPublish,
            enabled = !busy,
            modifier = Modifier.testTag(publishTag),
        ) {
            Text(text = stringResource(R.string.profile_publish))
        }
        OutlinedButton(
            onClick = onClear,
            enabled = !busy,
            modifier = Modifier.testTag(clearTag),
        ) {
            Text(text = stringResource(R.string.profile_clear))
        }
    }
    FeedbackText(feedback = feedback)
}

@Composable
private fun FeedbackText(feedback: ProfileFeedback?, testTag: String? = null) {
    val message = feedback?.let { stringResource(it.messageRes()) } ?: return
    val isError = feedback == ProfileFeedback.FAILED ||
        feedback == ProfileFeedback.OFFLINE ||
        feedback == ProfileFeedback.INVALID
    Text(
        text = message,
        style = MaterialTheme.typography.bodySmall,
        color = if (isError) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant,
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

@Composable
private fun SectionHeader(text: String) {
    Text(
        text = text,
        style = MaterialTheme.typography.titleSmall,
        color = MaterialTheme.colorScheme.primary,
        modifier = Modifier.padding(top = 16.dp, bottom = 4.dp),
    )
}

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

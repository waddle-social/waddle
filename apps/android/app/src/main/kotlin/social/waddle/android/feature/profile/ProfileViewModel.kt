package social.waddle.android.feature.profile

import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import social.waddle.android.AppGraph
import social.waddle.android.client.ConnectionState
import social.waddle.android.client.VerbResult
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.jid.bareJidOf
import social.waddle.android.viewModelFactoryOf
import social.waddle.client.ffi.WaddleAvatar
import social.waddle.client.ffi.WaddleTune
import social.waddle.client.ffi.WaddleVCard4

/** Web `VCardEditor.vue` field limits, enforced at the draft setters. */
private const val MAX_FULL_NAME = 160
private const val MAX_NICKNAME = 80
private const val MAX_PRONOUNS = 48
private const val MAX_NOTE = 500
private const val MAX_URL = 240
private const val MAX_STATUS_TEXT = 160

/**
 * Profile screen state: the vCard4 editor with optimistic save +
 * rollback (web `VCardEditor.vue` parity), the mood/activity/tune
 * status sections (web `UserSettingsPage.vue` parity, tune debounced
 * by the session manager), and the XEP-0084 avatar actions.
 */
class ProfileViewModel(
    private val sessionManager: XmppSessionManager,
    ownJid: String?,
    /** REST fallback avatar; XMPP bytes from [selfAvatar] win. */
    val restAvatarUrl: String?,
) : ViewModel() {
    private val ownBareJid: String? = ownJid?.let(::bareJidOf)

    private val _uiState = MutableStateFlow(ProfileUiState())
    val uiState: StateFlow<ProfileUiState> = _uiState.asStateFlow()

    /** The account's current XMPP avatar (preferred over [restAvatarUrl]). */
    val selfAvatar: StateFlow<WaddleAvatar?> = sessionManager.profileStore.avatars
        .map { avatars -> ownBareJid?.let(avatars::get) }
        .stateIn(viewModelScope, SharingStarted.Eagerly, null)

    private var loadedOnce = false

    /** The tune handed to the debounced publish, matched against the
     *  store to flip SCHEDULED feedback to PUBLISHED. */
    private var pendingTune: WaddleTune? = null

    init {
        viewModelScope.launch {
            // Load on every Ready transition until it succeeds: an
            // early fetch racing the connect must retry on reconnect
            // instead of gating the editor forever (web loads on mount
            // and gates saves on a successful load).
            sessionManager.connectionState.collect { state ->
                if (state is ConnectionState.Ready && !loadedOnce) load()
            }
        }
        viewModelScope.launch {
            sessionManager.profileStore.selfTune.collect { tune ->
                if (pendingTune != null && tune == pendingTune) {
                    pendingTune = null
                    _uiState.update { it.copy(tune = it.tune.copy(feedback = ProfileFeedback.PUBLISHED)) }
                }
            }
        }
    }

    private suspend fun load() {
        _uiState.update { it.copy(vcard = it.vcard.copy(loading = true, loadFailed = false)) }
        if (sessionManager.loadSelfProfile() != VerbResult.Ok) {
            _uiState.update { it.copy(vcard = it.vcard.copy(loading = false, loadFailed = true)) }
            return
        }
        loadedOnce = true
        applyVcardDraft(sessionManager.profileStore.selfVcard.value)
        seedStatusDrafts()
        _uiState.update { it.copy(vcard = it.vcard.copy(loading = false, loaded = true)) }
    }

    // ── vCard4 ────────────────────────────────────────────────────────

    fun setFullName(value: String) = updateVcard { it.copy(fullName = value.take(MAX_FULL_NAME)) }

    fun setNickname(value: String) = updateVcard { it.copy(nickname = value.take(MAX_NICKNAME)) }

    fun setPronouns(value: String) = updateVcard { it.copy(pronouns = value.take(MAX_PRONOUNS)) }

    fun setNote(value: String) = updateVcard { it.copy(note = value.take(MAX_NOTE)) }

    fun setUrl(value: String) = updateVcard { it.copy(url = value.take(MAX_URL)) }

    /**
     * Publish the draft optimistically: the store (and every reader of
     * it) shows the new profile immediately; a refusal rolls both the
     * store and this draft back to the last persisted value.
     */
    fun saveVcard() {
        val state = _uiState.value.vcard
        if (!state.loaded || state.saving) return
        val next = vcardFromDraft(state)
        if (next == persistedVcard()) {
            _uiState.update { it.copy(vcard = it.vcard.copy(feedback = ProfileFeedback.UNCHANGED)) }
            return
        }
        _uiState.update { it.copy(vcard = it.vcard.copy(saving = true, feedback = null)) }
        viewModelScope.launch {
            when (val result = sessionManager.publishProfile(next)) {
                VerbResult.Ok ->
                    _uiState.update {
                        it.copy(vcard = it.vcard.copy(saving = false, feedback = ProfileFeedback.PUBLISHED))
                    }
                else -> {
                    // The manager already rolled the store back; pull
                    // the rolled-back value into the draft (web parity).
                    applyVcardDraft(sessionManager.profileStore.selfVcard.value)
                    _uiState.update {
                        it.copy(vcard = it.vcard.copy(saving = false, feedback = feedbackFor(result)))
                    }
                }
            }
        }
    }

    /** Discard edits: reset the draft to the last persisted profile. */
    fun revertVcard() {
        applyVcardDraft(sessionManager.profileStore.selfVcard.value)
        _uiState.update { it.copy(vcard = it.vcard.copy(feedback = null)) }
    }

    // ── Mood (XEP-0107) ───────────────────────────────────────────────

    fun setMoodKind(kind: String) =
        _uiState.update { it.copy(mood = it.mood.copy(draft = it.mood.draft.copy(kind = kind))) }

    fun setMoodText(text: String) = _uiState.update {
        it.copy(mood = it.mood.copy(draft = it.mood.draft.copy(text = text.take(MAX_STATUS_TEXT))))
    }

    fun publishMood() {
        val publication = buildMoodPublication(_uiState.value.mood.draft)
        if (publication == null) {
            _uiState.update { it.copy(mood = it.mood.copy(feedback = ProfileFeedback.INVALID)) }
            return
        }
        _uiState.update { it.copy(mood = it.mood.copy(busy = true, feedback = null)) }
        viewModelScope.launch {
            val result = sessionManager.setMood(publication.kind, publication.text)
            _uiState.update {
                it.copy(mood = it.mood.copy(busy = false, feedback = feedbackFor(result)))
            }
        }
    }

    fun clearMood() {
        _uiState.update { it.copy(mood = it.mood.copy(busy = true, feedback = null)) }
        viewModelScope.launch {
            val result = sessionManager.clearMood()
            _uiState.update {
                val draft = if (result == VerbResult.Ok) MoodDraft() else it.mood.draft
                it.copy(mood = it.mood.copy(busy = false, draft = draft, feedback = clearFeedbackFor(result)))
            }
        }
    }

    // ── Activity (XEP-0108) ───────────────────────────────────────────

    fun setActivityGeneral(general: String) = _uiState.update {
        it.copy(activity = it.activity.copy(draft = it.activity.draft.copy(general = general)))
    }

    fun setActivitySpecific(specific: String) = _uiState.update {
        it.copy(activity = it.activity.copy(draft = it.activity.draft.copy(specific = specific.take(64))))
    }

    fun setActivityText(text: String) = _uiState.update {
        it.copy(activity = it.activity.copy(draft = it.activity.draft.copy(text = text.take(MAX_STATUS_TEXT))))
    }

    fun publishActivity() {
        val build = buildActivityPublication(_uiState.value.activity.draft)
        val publication = build.publication
        if (publication == null) {
            _uiState.update {
                it.copy(activity = it.activity.copy(errors = build.errors, feedback = ProfileFeedback.INVALID))
            }
            return
        }
        _uiState.update {
            it.copy(activity = it.activity.copy(errors = emptySet(), busy = true, feedback = null))
        }
        viewModelScope.launch {
            val result = sessionManager.setActivity(publication.general, publication.specific, publication.text)
            _uiState.update { state ->
                val draft = if (result == VerbResult.Ok) {
                    // Show the normalized identifiers that were published.
                    state.activity.draft.copy(
                        specific = publication.specific.orEmpty(),
                        text = publication.text.orEmpty(),
                    )
                } else {
                    state.activity.draft
                }
                state.copy(
                    activity = state.activity.copy(busy = false, draft = draft, feedback = feedbackFor(result)),
                )
            }
        }
    }

    fun clearActivity() {
        _uiState.update { it.copy(activity = it.activity.copy(errors = emptySet(), busy = true, feedback = null)) }
        viewModelScope.launch {
            val result = sessionManager.clearActivity()
            _uiState.update {
                val draft = if (result == VerbResult.Ok) ActivityDraft() else it.activity.draft
                it.copy(
                    activity = it.activity.copy(busy = false, draft = draft, feedback = clearFeedbackFor(result)),
                )
            }
        }
    }

    // ── Tune (XEP-0118) ───────────────────────────────────────────────

    fun setTuneField(field: TuneField, value: String) = _uiState.update {
        it.copy(tune = it.tune.copy(draft = field.applyTo(it.tune.draft, value)))
    }

    /** Hand the tune to the manager's debounced publish; feedback flips
     *  from SCHEDULED to PUBLISHED once the store confirms the send. */
    fun publishTune() {
        val build = buildTunePublication(_uiState.value.tune.draft)
        val publication = build.publication
        if (publication == null) {
            _uiState.update {
                it.copy(tune = it.tune.copy(errors = build.errors, feedback = ProfileFeedback.INVALID))
            }
            return
        }
        pendingTune = publication
        sessionManager.setTune(publication)
        _uiState.update {
            it.copy(tune = it.tune.copy(errors = emptySet(), feedback = ProfileFeedback.SCHEDULED))
        }
    }

    fun clearTune() {
        pendingTune = null
        _uiState.update { it.copy(tune = it.tune.copy(errors = emptySet(), busy = true, feedback = null)) }
        viewModelScope.launch {
            val result = sessionManager.clearTune()
            _uiState.update {
                val draft = if (result == VerbResult.Ok) TuneDraft() else it.tune.draft
                it.copy(tune = it.tune.copy(busy = false, draft = draft, feedback = clearFeedbackFor(result)))
            }
        }
    }

    // ── Avatar (XEP-0084) ─────────────────────────────────────────────

    /** Publish a processed pick; `null` means the image could not be read. */
    fun publishAvatar(processed: ProcessedAvatar?) {
        if (processed == null) {
            _uiState.update { it.copy(avatar = it.avatar.copy(feedback = ProfileFeedback.FAILED)) }
            return
        }
        _uiState.update { it.copy(avatar = it.avatar.copy(busy = true, feedback = null)) }
        viewModelScope.launch {
            val result = sessionManager.publishAvatar(
                processed.data,
                processed.mimeType,
                processed.width.toUInt(),
                processed.height.toUInt(),
            )
            _uiState.update {
                it.copy(avatar = it.avatar.copy(busy = false, feedback = feedbackFor(result)))
            }
        }
    }

    fun removeAvatar() {
        _uiState.update { it.copy(avatar = it.avatar.copy(busy = true, feedback = null)) }
        viewModelScope.launch {
            val result = sessionManager.disableAvatar()
            _uiState.update {
                it.copy(avatar = it.avatar.copy(busy = false, feedback = clearFeedbackFor(result)))
            }
        }
    }

    // ── Shared plumbing ───────────────────────────────────────────────

    private fun updateVcard(transform: (VcardSectionState) -> VcardSectionState) {
        _uiState.update { it.copy(vcard = transform(it.vcard)) }
    }

    private fun applyVcardDraft(vcard: WaddleVCard4?) = updateVcard {
        it.copy(
            fullName = vcard?.fullName.orEmpty(),
            nickname = vcard?.nickname.orEmpty(),
            pronouns = vcard?.pronouns.orEmpty(),
            note = vcard?.note.orEmpty(),
            url = vcard?.url.orEmpty(),
        )
    }

    private fun seedStatusDrafts() {
        val store = sessionManager.profileStore
        val mood = store.selfMood.value
        val activity = store.selfActivity.value
        val tune = store.selfTune.value
        _uiState.update { state ->
            state.copy(
                mood = state.mood.copy(
                    draft = MoodDraft(kind = mood?.kind.orEmpty(), text = mood?.text.orEmpty()),
                ),
                activity = state.activity.copy(
                    draft = ActivityDraft(
                        general = activity?.general.orEmpty(),
                        specific = activity?.specific.orEmpty(),
                        text = activity?.text.orEmpty(),
                    ),
                ),
                tune = state.tune.copy(draft = tuneDraftOf(tune)),
            )
        }
    }

    private fun tuneDraftOf(tune: WaddleTune?): TuneDraft = TuneDraft(
        artist = tune?.artist.orEmpty(),
        title = tune?.title.orEmpty(),
        source = tune?.source.orEmpty(),
        length = tune?.lengthSeconds?.toString().orEmpty(),
        rating = tune?.rating?.toString().orEmpty(),
        track = tune?.track.orEmpty(),
        uri = tune?.uri.orEmpty(),
    )

    private fun vcardFromDraft(state: VcardSectionState): WaddleVCard4 = WaddleVCard4(
        fullName = state.fullName.trim().ifEmpty { null },
        nickname = state.nickname.trim().ifEmpty { null },
        pronouns = state.pronouns.trim().ifEmpty { null },
        note = state.note.trim().ifEmpty { null },
        url = state.url.trim().ifEmpty { null },
        // Not edited here: the photo URI belongs to the avatar flow.
        photoUri = sessionManager.profileStore.selfVcard.value?.photoUri,
    )

    private fun persistedVcard(): WaddleVCard4 =
        sessionManager.profileStore.selfVcard.value
            ?: WaddleVCard4(
                fullName = null,
                nickname = null,
                pronouns = null,
                note = null,
                url = null,
                photoUri = null,
            )

    private fun feedbackFor(result: VerbResult): ProfileFeedback = when (result) {
        VerbResult.Ok -> ProfileFeedback.PUBLISHED
        VerbResult.NotConnected -> ProfileFeedback.OFFLINE
        else -> ProfileFeedback.FAILED
    }

    private fun clearFeedbackFor(result: VerbResult): ProfileFeedback = when (result) {
        VerbResult.Ok -> ProfileFeedback.CLEARED
        VerbResult.NotConnected -> ProfileFeedback.OFFLINE
        else -> ProfileFeedback.FAILED
    }

    /** The seven tune draft fields, addressed by one setter. */
    enum class TuneField {
        ARTIST, TITLE, SOURCE, LENGTH, RATING, TRACK, URI;

        fun applyTo(draft: TuneDraft, value: String): TuneDraft = when (this) {
            ARTIST -> draft.copy(artist = value)
            TITLE -> draft.copy(title = value)
            SOURCE -> draft.copy(source = value)
            LENGTH -> draft.copy(length = value)
            RATING -> draft.copy(rating = value)
            TRACK -> draft.copy(track = value)
            URI -> draft.copy(uri = value)
        }
    }

    companion object {
        fun factory(graph: AppGraph): ViewModelProvider.Factory = viewModelFactoryOf {
            ProfileViewModel(
                sessionManager = graph.sessionManager,
                ownJid = graph.currentSession.value?.jid,
                restAvatarUrl = graph.currentSession.value?.avatarUrl,
            )
        }
    }
}

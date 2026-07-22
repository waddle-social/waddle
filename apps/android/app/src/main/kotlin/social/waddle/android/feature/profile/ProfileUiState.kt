package social.waddle.android.feature.profile

/** Outcome of the last publish/clear on one profile section, rendered
 *  as inline feedback text by the screen. */
enum class ProfileFeedback {
    /** The server accepted the publish. */
    PUBLISHED,

    /** The retraction went through. */
    CLEARED,

    /** The draft matches the saved profile — nothing to publish. */
    UNCHANGED,

    /** Local validation failed; field errors carry the specifics. */
    INVALID,

    /** A live session refused the publish or the transport broke. */
    FAILED,

    /** No live session existed to carry the verb. */
    OFFLINE,
}

/** XEP-0292 vCard4 editor state (web `VCardEditor.vue` parity). */
data class VcardSectionState(
    val loading: Boolean = true,
    /** The initial fetch failed; publishing stays gated until a reload. */
    val loadFailed: Boolean = false,
    /** The saved profile arrived — publishing is allowed. */
    val loaded: Boolean = false,
    val fullName: String = "",
    val nickname: String = "",
    val pronouns: String = "",
    val note: String = "",
    val url: String = "",
    val saving: Boolean = false,
    val feedback: ProfileFeedback? = null,
)

data class MoodSectionState(
    val draft: MoodDraft = MoodDraft(),
    val busy: Boolean = false,
    val feedback: ProfileFeedback? = null,
)

data class ActivitySectionState(
    val draft: ActivityDraft = ActivityDraft(),
    val errors: Set<ActivityFieldError> = emptySet(),
    val busy: Boolean = false,
    val feedback: ProfileFeedback? = null,
)

data class TuneSectionState(
    val draft: TuneDraft = TuneDraft(),
    val errors: Set<TuneFieldError> = emptySet(),
    val busy: Boolean = false,
    val feedback: ProfileFeedback? = null,
)

data class AvatarSectionState(
    val busy: Boolean = false,
    val feedback: ProfileFeedback? = null,
)

data class ProfileUiState(
    val vcard: VcardSectionState = VcardSectionState(),
    val mood: MoodSectionState = MoodSectionState(),
    val activity: ActivitySectionState = ActivitySectionState(),
    val tune: TuneSectionState = TuneSectionState(),
    val avatar: AvatarSectionState = AvatarSectionState(),
)

package social.waddle.android.ui.huddle

import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp

/** Huddle avatar sizes (web `avatar` recipe). */
enum class AvatarSize(val dp: Dp) {
    /** Overlapping stacks: call participants on a card, thread chips. */
    XS(24.dp),
    SM(28.dp),
    MD(32.dp),

    /** Message rows (web 44.5px, tightened for a phone column). */
    MESSAGE(40.dp),
    LG(44.dp),
    XL(64.dp),
}

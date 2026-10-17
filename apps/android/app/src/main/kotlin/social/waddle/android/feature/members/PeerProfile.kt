package social.waddle.android.feature.members

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.ChatBubbleOutline
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import social.waddle.android.R
import social.waddle.android.client.store.PresenceShow
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.KickerText
import social.waddle.android.ui.huddle.AvatarSize
import social.waddle.android.ui.huddle.HuddleAvatar
import social.waddle.android.ui.huddle.LiveDot
import social.waddle.android.ui.huddle.PillButton
import social.waddle.android.ui.huddle.glow
import social.waddle.client.ffi.WaddleMucAffiliation

/**
 * What a profile sheet knows about someone. Every surface fills in only
 * what it has: a member row knows the room role, a message author may
 * be no more than a nick.
 */
@Immutable
data class PeerProfile(
    val displayName: String,
    /** Real bare JID; `null` for an anonymous room occupant. */
    val jid: String?,
    /** Their Show; `null` when not seen anywhere this stream. */
    val presence: PresenceShow? = null,
    /** In this room's huddle (XEP-0272 Muji) right now. */
    val inHuddle: Boolean = false,
    /** Their presence `<status/>` text. */
    val statusText: String? = null,
    /** Room affiliation, when the surface knows it. */
    val affiliation: WaddleMucAffiliation? = null,
    /** XEP-0317 hat titles. */
    val hats: List<String> = emptyList(),
)

/**
 * Someone's profile as a bottom sheet (web member profile sheet): who
 * they are, where they are — a glowing ember banner while in the huddle,
 * else [presenceLine] — their status text, "Message" when [onMessage]
 * can open a DM, and [manage] for surfaces that already offer owner and
 * admin verbs for this person.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun PeerProfileSheet(
    profile: PeerProfile,
    presenceLine: String?,
    onDismiss: () -> Unit,
    onMessage: (() -> Unit)?,
    manage: (@Composable ColumnScope.() -> Unit)? = null,
) {
    ModalBottomSheet(onDismissRequest = onDismiss, containerColor = Waddle.colors.surface) {
        PeerProfileContent(profile = profile, presenceLine = presenceLine, onMessage = onMessage)
        manage?.invoke(this)
    }
}

/** The sheet's body without the sheet, for hosts that draw their own (goldens). */
@Composable
fun PeerProfileContent(profile: PeerProfile, presenceLine: String?, onMessage: (() -> Unit)?) {
    val colors = Waddle.colors
    Column(
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(Waddle.space.xxs),
        modifier = Modifier
            .fillMaxWidth()
            .navigationBarsPadding()
            .padding(bottom = Waddle.space.md),
    ) {
        KickerText(text = stringResource(R.string.huddle_member_profile))
        HuddleAvatar(
            jid = profile.jid,
            name = profile.displayName,
            size = AvatarSize.XL,
            presence = profile.presence,
            inHuddle = profile.inHuddle,
            ground = colors.surface,
            modifier = Modifier.padding(vertical = Waddle.space.sm),
        )
        Text(
            text = profile.displayName,
            style = MaterialTheme.typography.headlineSmall,
            color = colors.fg,
            textAlign = TextAlign.Center,
            modifier = Modifier
                .padding(horizontal = Waddle.space.md)
                .semantics { heading() },
        )
        profile.jid?.let {
            Text(text = it, style = Waddle.type.time, color = colors.fgMuted, maxLines = 1)
        }
        if (profile.affiliation != null || profile.hats.isNotEmpty()) {
            Row(modifier = Modifier.padding(top = Waddle.space.xxs)) {
                RoleLine(affiliation = profile.affiliation, hats = profile.hats)
            }
        }
        if (profile.inHuddle) {
            HuddleBanner(modifier = Modifier.padding(top = Waddle.space.sm))
        } else {
            presenceLine?.let { Text(text = it, style = Waddle.type.control, color = colors.fgMuted) }
        }
        profile.statusText?.let { status ->
            Text(
                text = status,
                style = Waddle.type.body,
                color = colors.fg,
                textAlign = TextAlign.Center,
                maxLines = STATUS_MAX_LINES,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.padding(horizontal = Waddle.space.lg, vertical = Waddle.space.xxs),
            )
        }
        onMessage?.let {
            PillButton(
                text = stringResource(R.string.huddle_member_message),
                onClick = it,
                icon = Icons.Outlined.ChatBubbleOutline,
                modifier = Modifier.padding(top = Waddle.space.md),
            )
        }
    }
}

/** Affiliation and hats as mono kickers: owner gold, admin teal, banned danger. */
@Composable
internal fun RoleLine(affiliation: WaddleMucAffiliation?, hats: List<String>) {
    val colors = Waddle.colors
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs),
    ) {
        affiliation?.let { KickerText(text = affiliationLabel(it), color = affiliationColor(it)) }
        hats.forEach { hat -> KickerText(text = hat, color = colors.actionText) }
    }
}

@Composable
private fun affiliationColor(affiliation: WaddleMucAffiliation): Color {
    val colors = Waddle.colors
    return when (affiliation) {
        WaddleMucAffiliation.OWNER -> colors.goldText
        WaddleMucAffiliation.ADMIN -> colors.actionText
        WaddleMucAffiliation.OUTCAST -> colors.dangerText
        WaddleMucAffiliation.MEMBER, WaddleMucAffiliation.NONE -> colors.fgMuted
    }
}

/** Wire value stays `outcast`; only the label reads "Banned" (web parity). */
@Composable
private fun affiliationLabel(affiliation: WaddleMucAffiliation): String = stringResource(
    when (affiliation) {
        WaddleMucAffiliation.OWNER -> R.string.affiliation_owner
        WaddleMucAffiliation.ADMIN -> R.string.affiliation_admin
        WaddleMucAffiliation.MEMBER -> R.string.affiliation_member
        WaddleMucAffiliation.OUTCAST -> R.string.affiliation_banned
        WaddleMucAffiliation.NONE -> R.string.affiliation_none
    },
)

@Composable
private fun HuddleBanner(modifier: Modifier = Modifier) {
    val colors = Waddle.colors
    val shape = RoundedCornerShape(Waddle.radius.panel)
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs),
        modifier = modifier
            .padding(horizontal = Waddle.space.md)
            .glow(colors.glowLive, blur = BANNER_GLOW, cornerRadius = Waddle.radius.panel, spread = BANNER_GLOW_SPREAD)
            .background(colors.liveSubtle, shape)
            .border(BorderStroke(1.dp, colors.live), shape)
            .padding(horizontal = Waddle.space.md, vertical = Waddle.space.xs),
    ) {
        LiveDot()
        Text(
            text = stringResource(R.string.huddle_member_in_huddle),
            style = Waddle.type.control,
            color = colors.liveText,
        )
    }
}

private const val STATUS_MAX_LINES = 3
private val BANNER_GLOW = 28.dp
private val BANNER_GLOW_SPREAD = (-10).dp

package social.waddle.android.feature.home

import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.graphics.PathEffect
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import social.waddle.android.R
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.huddle.Kicker

/** The `waddle.` wordmark: Space Grotesk, lowercase, an ember full stop. */
@Composable
fun Wordmark(modifier: Modifier = Modifier) {
    val appName = stringResource(R.string.app_name)
    Text(
        text = buildAnnotatedString {
            append(stringResource(R.string.shell_wordmark))
            withStyle(SpanStyle(color = Waddle.colors.live)) { append(".") }
        },
        style = MaterialTheme.typography.headlineMedium,
        color = Waddle.colors.fg,
        maxLines = 1,
        modifier = modifier.semantics {
            contentDescription = appName
            heading()
        },
    )
}

/**
 * A page's opening (web `community-page__header`): a mono kicker, the
 * page title in Space Grotesk and one lead line, with an optional action
 * on the right. The title is the screen's heading.
 */
@Composable
fun PageHeader(
    title: String,
    kicker: String,
    lead: String,
    modifier: Modifier = Modifier,
    live: Boolean = false,
    action: @Composable () -> Unit = {},
) {
    Column(modifier = modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(Waddle.space.xs)) {
        Kicker(text = kicker, live = live)
        Row(
            horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                text = title,
                style = Waddle.type.headline,
                color = Waddle.colors.fg,
                modifier = Modifier
                    .weight(1f)
                    .semantics { heading() },
            )
            action()
        }
        Text(text = lead, style = MaterialTheme.typography.bodyMedium, color = Waddle.colors.fgMuted)
    }
}

/**
 * A list section's label: a mono kicker heading, ember with a live dot
 * for what is happening now, and an optional caption at the far end.
 */
@Composable
fun SectionLabel(
    text: String,
    modifier: Modifier = Modifier,
    live: Boolean = false,
    trailing: String? = null,
) {
    Row(
        modifier = modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
    ) {
        Kicker(text = text, live = live, heading = true, modifier = Modifier.weight(1f))
        if (trailing != null) {
            Text(text = trailing, style = MaterialTheme.typography.bodySmall, color = Waddle.colors.fgMuted)
        }
    }
}

/**
 * Search over a list (web "Find people": radius 10 on the ground, 1dp
 * border). A field, not a banner: card colour, a search glyph, a clear
 * button once there is text, 44dp tall so the whole field is a target.
 */
@Composable
fun ShellSearchField(
    value: String,
    onValueChange: (String) -> Unit,
    placeholder: String,
    modifier: Modifier = Modifier,
) {
    val colors = Waddle.colors
    val textStyle = MaterialTheme.typography.bodyLarge.copy(color = colors.fg)
    BasicTextField(
        value = value,
        onValueChange = onValueChange,
        singleLine = true,
        textStyle = textStyle,
        cursorBrush = SolidColor(colors.action),
        keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
        modifier = modifier
            .fillMaxWidth()
            .semantics { contentDescription = placeholder },
        decorationBox = { inner ->
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs),
                modifier = Modifier
                    .heightIn(min = Waddle.control.touch)
                    .border(1.dp, colors.border, RoundedCornerShape(Waddle.radius.control))
                    .padding(start = Waddle.space.sm),
            ) {
                Icon(
                    Icons.Outlined.Search,
                    contentDescription = null,
                    tint = colors.fgMuted,
                    modifier = Modifier.size(SEARCH_ICON),
                )
                Box(Modifier.weight(1f)) {
                    if (value.isEmpty()) {
                        Text(text = placeholder, style = textStyle, color = colors.fgSoft, maxLines = 1)
                    }
                    inner()
                }
                if (value.isNotEmpty()) {
                    IconButton(onClick = { onValueChange("") }) {
                        Icon(
                            Icons.Outlined.Close,
                            contentDescription = stringResource(R.string.shell_clear_search),
                            tint = colors.fgMuted,
                            modifier = Modifier.size(SEARCH_ICON),
                        )
                    }
                }
            }
        },
    )
}

/**
 * The quiet box for "nothing yet" and "finding…" (web `community-empty`):
 * a dashed outline, centred muted copy. Never a big glyph and a zero.
 */
@Composable
fun DashedNote(text: String, modifier: Modifier = Modifier, content: @Composable () -> Unit = {}) {
    val colors = Waddle.colors
    val radius = Waddle.radius.card
    Column(
        modifier = modifier
            .fillMaxWidth()
            .drawBehind {
                val stroke = 1.dp.toPx()
                val dash = DASH.toPx()
                drawRoundRect(
                    color = colors.border,
                    cornerRadius = CornerRadius(radius.toPx()),
                    style = Stroke(width = stroke, pathEffect = PathEffect.dashPathEffect(floatArrayOf(dash, dash))),
                )
            }
            .padding(horizontal = Waddle.space.md, vertical = Waddle.space.lg),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(Waddle.space.md),
    ) {
        Text(
            text = text,
            style = MaterialTheme.typography.bodyMedium,
            color = colors.fgMuted,
            textAlign = TextAlign.Center,
        )
        content()
    }
}

private val SEARCH_ICON = 20.dp
private val DASH = 6.dp

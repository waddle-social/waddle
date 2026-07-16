package social.waddle.android.feature.conversation

import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.LinkAnnotation
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextLinkStyles
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.withLink
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import social.waddle.android.client.RichBlock
import social.waddle.android.client.RichInlineRun

/**
 * Rich message body: the block/run model from `richBlocksOf` rendered
 * with web `MessageBody.vue` parity — inline bold/italic/strike/code
 * marks, scheme-gated tappable links, mention highlighting, full-width
 * monospace code blocks, and accent-barred blockquotes. Received
 * bodies are NEVER markdown-parsed; everything here comes from typed
 * XEP-0394 markup and XEP-0372 references.
 */
@Composable
fun RichMessageBody(blocks: List<RichBlock>, modifier: Modifier = Modifier) {
    Column(
        modifier = modifier,
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        blocks.forEach { block ->
            when (block) {
                is RichBlock.Paragraph -> ParagraphText(block)
                is RichBlock.CodeBlock -> CodeBlockSurface(block)
                is RichBlock.Blockquote -> BlockquoteColumn(block)
            }
        }
    }
}

@Composable
private fun ParagraphText(paragraph: RichBlock.Paragraph) {
    if (paragraph.runs.isEmpty()) return
    Text(
        text = annotatedParagraph(paragraph.runs),
        style = MaterialTheme.typography.bodyLarge,
    )
}

/** Full-width monospace block; long lines scroll, never wrap (web `<pre>`). */
@Composable
private fun CodeBlockSurface(block: RichBlock.CodeBlock) {
    Surface(
        shape = RoundedCornerShape(6.dp),
        color = MaterialTheme.colorScheme.surfaceContainerHighest,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Text(
            text = block.text,
            style = MaterialTheme.typography.bodySmall,
            fontFamily = FontFamily.Monospace,
            modifier = Modifier
                .horizontalScroll(rememberScrollState())
                .padding(8.dp),
        )
    }
}

/** Accent bar + inset quote paragraphs (web blockquote styling). */
@Composable
private fun BlockquoteColumn(quote: RichBlock.Blockquote) {
    Row(modifier = Modifier.height(IntrinsicSize.Min)) {
        Box(
            modifier = Modifier
                .width(3.dp)
                .fillMaxHeight()
                .background(
                    color = MaterialTheme.colorScheme.primary,
                    shape = RoundedCornerShape(1.dp),
                ),
        )
        Column(
            modifier = Modifier.padding(start = 8.dp),
            verticalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            quote.paragraphs.forEach { ParagraphText(it) }
        }
    }
}

@Composable
private fun annotatedParagraph(runs: List<RichInlineRun>): AnnotatedString {
    val uriHandler = LocalUriHandler.current
    val mentionColor = MaterialTheme.colorScheme.primary
    val linkColor = MaterialTheme.colorScheme.primary
    val codeBackground = MaterialTheme.colorScheme.surfaceContainerHighest
    return buildAnnotatedString {
        runs.forEach { run ->
            val style = run.spanStyle(mentionColor, codeBackground)
            val linkUri = run.linkUri
            if (linkUri != null) {
                val linkStyle = style.copy(
                    color = linkColor,
                    textDecoration = mergedDecoration(style.textDecoration, TextDecoration.Underline),
                )
                // Peer-controlled URI (already safeUri-gated): openUri can
                // still throw when no activity handles it — never crash.
                val link = LinkAnnotation.Url(linkUri, TextLinkStyles(style = linkStyle)) {
                    runCatching { uriHandler.openUri(linkUri) }
                }
                withLink(link) { append(run.text) }
            } else {
                withStyle(style) { append(run.text) }
            }
        }
    }
}

private fun RichInlineRun.spanStyle(mentionColor: Color, codeBackground: Color): SpanStyle =
    SpanStyle(
        fontWeight = when {
            bold -> FontWeight.Bold
            mentionUri != null -> FontWeight.Medium
            else -> null
        },
        fontStyle = if (italic) FontStyle.Italic else null,
        textDecoration = if (strikethrough) TextDecoration.LineThrough else null,
        fontFamily = if (code) FontFamily.Monospace else null,
        background = if (code) codeBackground else Color.Unspecified,
        color = if (mentionUri != null) mentionColor else Color.Unspecified,
    )

private fun mergedDecoration(base: TextDecoration?, extra: TextDecoration): TextDecoration =
    if (base == null) extra else TextDecoration.combine(listOf(base, extra))

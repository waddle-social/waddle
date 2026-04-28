package social.waddle.android.ui.auth

import android.content.ActivityNotFoundException
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.browser.customtabs.CustomTabsIntent
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import social.waddle.android.auth.AuthState

/**
 * The Apple app opens the verification URL via SwiftUI `OpenURLAction`,
 * which lands in Safari and shares cookies/state with the rest of the
 * system. The Android equivalent for OAuth flows is **Chrome Custom
 * Tabs** — not raw `Intent.ACTION_VIEW`. Custom Tabs:
 *
 *  - share session cookies with the user's Chrome (so an existing
 *    colony sign-in is reused),
 *  - hand redirects back to the calling app cleanly, and
 *  - never resolve to a third-party intent handler that might mishandle
 *    the URL.
 *
 * Falls back to `ACTION_VIEW` if Custom Tabs aren't available, and
 * exposes a "copy URL" button as a manual fallback.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun DeviceCodeScreen(
    state: AuthState.AwaitingDevice,
    onCancel: () -> Unit,
) {
    val context = LocalContext.current
    val displayLink = remember(state.flow.verificationUriComplete, state.flow.verificationUri) {
        normalizeVerificationUrl(state.flow.verificationUriComplete)
            ?: normalizeVerificationUrl(state.flow.verificationUri)
    }

    Scaffold(
        topBar = { TopAppBar(title = { Text("Open the verification page") }) },
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .padding(24.dp)
                .verticalScroll(rememberScrollState()),
            verticalArrangement = Arrangement.spacedBy(20.dp),
        ) {
            Text(
                text = "Approve sign-in for ${state.provider.displayName ?: state.provider.id}",
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.Medium,
            )

            Surface(
                modifier = Modifier.fillMaxWidth(),
                shape = RoundedCornerShape(16.dp),
                color = MaterialTheme.colorScheme.surfaceVariant,
            ) {
                Column(
                    modifier = Modifier.padding(20.dp),
                    verticalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    Text(
                        text = "Your code",
                        style = MaterialTheme.typography.labelMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    Text(
                        text = state.flow.userCode,
                        style = MaterialTheme.typography.headlineLarge,
                        fontWeight = FontWeight.Bold,
                        modifier = Modifier.fillMaxWidth(),
                        textAlign = TextAlign.Center,
                    )
                }
            }

            displayLink?.let { link ->
                Button(
                    onClick = { openInBrowser(context, link) },
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text("Open verification page")
                }
                TextButton(
                    onClick = { copyToClipboard(context, "Verification URL", link) },
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text("Copy verification URL")
                }
                Text(
                    text = link,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }

            Text(
                text = "Sign in in the browser. This screen returns automatically once the server confirms.",
                style = MaterialTheme.typography.bodyMedium,
            )

            state.errorMessage?.let { message ->
                Text(
                    text = message,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                )
            }

            OutlinedButton(
                onClick = onCancel,
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text("Cancel")
            }
        }
    }
}

private fun openInBrowser(context: Context, url: String) {
    val uri = runCatching { Uri.parse(url) }.getOrNull() ?: return
    // Chrome Custom Tabs first — proper OAuth UX, shared cookies. Fall
    // back to a plain ACTION_VIEW so the user is never stuck if Custom
    // Tabs aren't available (e.g., no Chrome / no Custom Tabs service).
    val customTabs = CustomTabsIntent.Builder()
        .setShowTitle(true)
        .build()
    customTabs.intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
    runCatching { customTabs.launchUrl(context, uri) }.onFailure {
        runCatching {
            context.startActivity(
                Intent(Intent.ACTION_VIEW, uri).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
            )
        }.onFailure { error ->
            if (error is ActivityNotFoundException) {
                // Last resort: clipboard so the user can paste it manually.
                copyToClipboard(context, "Verification URL", url)
            }
        }
    }
}

private fun copyToClipboard(context: Context, label: String, text: String) {
    val manager = context.getSystemService(Context.CLIPBOARD_SERVICE) as? ClipboardManager
    manager?.setPrimaryClip(ClipData.newPlainText(label, text))
}

/**
 * Mirrors the iOS `normalizedVerificationURL(from:)` helper — strips
 * stray quote escapes that can appear when a URL is double-JSON-encoded
 * upstream. The live server does not currently emit these but the iOS
 * code carries the workaround so we match it.
 */
private fun normalizeVerificationUrl(raw: String?): String? {
    val trimmed = raw?.trim().orEmpty()
    if (trimmed.isEmpty()) return null
    if (!trimmed.contains("%22") && !trimmed.contains("%5C%22") && !trimmed.contains("\\\"") &&
        !trimmed.contains("\"")
    ) {
        return trimmed
    }
    val decoded = runCatching { Uri.decode(trimmed) }.getOrDefault(trimmed)
    return decoded.replace("\\\"", "").replace("\"", "")
}

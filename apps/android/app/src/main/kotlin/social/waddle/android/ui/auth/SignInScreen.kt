package social.waddle.android.ui.auth

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ElevatedCard
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import social.waddle.android.auth.AuthState
import social.waddle.android.domain.auth.AuthProvider

/**
 * Form + provider list. Built as a single [LazyColumn] so the provider
 * items can scroll independently of (and without nesting under) any
 * outer scroll container — nested vertical scrollers are a Compose
 * measurement crash.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun SignInScreen(
    state: AuthState.SignedOut,
    onServerUrlChange: (String) -> Unit,
    onRetryProviders: () -> Unit,
    onSelectProvider: (AuthProvider) -> Unit,
) {
    Scaffold(
        topBar = { TopAppBar(title = { Text("Sign in to Waddle") }) },
    ) { padding ->
        LazyColumn(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .padding(horizontal = 24.dp),
            verticalArrangement = Arrangement.spacedBy(20.dp),
            contentPadding = androidx.compose.foundation.layout.PaddingValues(vertical = 24.dp),
        ) {
            item {
                OutlinedTextField(
                    value = state.serverUrl,
                    onValueChange = onServerUrlChange,
                    label = { Text("Server URL") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
            }

            item {
                Text(
                    text = "Choose a provider",
                    style = MaterialTheme.typography.titleMedium,
                    fontWeight = FontWeight.Medium,
                )
            }

            when {
                state.isLoadingProviders -> item { LoadingRow() }
                state.providers.isEmpty() -> item { EmptyProvidersCard(onRetryProviders) }
                else -> items(state.providers, key = { it.id }) { provider ->
                    Button(
                        onClick = { onSelectProvider(provider) },
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Text(provider.displayName ?: provider.id)
                    }
                }
            }

            state.errorMessage?.let { message ->
                item {
                    Text(
                        text = message,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.error,
                    )
                }
            }
        }
    }
}

@Composable
private fun LoadingRow() {
    Column(
        modifier = Modifier.fillMaxWidth(),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        CircularProgressIndicator(modifier = Modifier.size(32.dp))
        Text(
            text = "Loading providers…",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun EmptyProvidersCard(onRetry: () -> Unit) {
    ElevatedCard(modifier = Modifier.fillMaxWidth()) {
        Column(
            modifier = Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(
                text = "No providers available from this server.",
                style = MaterialTheme.typography.bodyMedium,
            )
            TextButton(onClick = onRetry) {
                Text("Retry")
            }
        }
    }
}

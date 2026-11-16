package social.waddle.android.feature.events

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import social.waddle.android.R
import java.time.ZoneId

/** Search is a draft; only selecting a valid zone changes the event. */
@Composable
internal fun EventTimeZoneField(value: String, onChange: (String) -> Unit, enabled: Boolean) {
    var open by rememberSaveable { mutableStateOf(false) }
    var query by rememberSaveable { mutableStateOf("") }
    val label = stringResource(R.string.events_timezone)
    val zones = remember { ZoneId.getAvailableZoneIds().filterNot { it.startsWith("SystemV/") }.sorted() }
    Column {
        Text(label, style = MaterialTheme.typography.labelLarge)
        OutlinedButton(
            onClick = {
                query = ""
                open = true
            }, enabled = enabled,
            modifier = Modifier.fillMaxWidth().semantics { contentDescription = label },
        ) {
            Column {
                Text(zoneCity(value))
                if (zoneCity(value) != value) Text(value, style = MaterialTheme.typography.labelSmall)
            }
        }
    }
    if (open) {
        val terms = query.trim().split(Regex("\\s+")).filter { it.isNotEmpty() }
        val matches = zones.filter { zone -> terms.all { zone.replace('_', ' ').contains(it, ignoreCase = true) } }
        AlertDialog(
            onDismissRequest = { open = false },
            title = { Text(label) },
            text = {
                Column {
                    OutlinedTextField(
                        query, { query = it }, label = { Text(stringResource(R.string.events_zone_search)) },
                        singleLine = true, modifier = Modifier.fillMaxWidth(),
                    )
                    LazyColumn(Modifier.heightIn(max = 360.dp)) {
                        items(matches, key = { it }) { zone ->
                            TextButton(onClick = {
                                onChange(zone)
                                open = false
                            }, enabled = enabled, modifier = Modifier.fillMaxWidth()) {
                                Column(Modifier.fillMaxWidth()) {
                                    Text(zoneCity(zone))
                                    Text(zone, style = MaterialTheme.typography.labelSmall)
                                }
                            }
                        }
                    }
                    if (matches.isEmpty()) Text(stringResource(R.string.events_no_zones))
                }
            },
            confirmButton = {},
            dismissButton = { TextButton(onClick = { open = false }) { Text(stringResource(R.string.events_cancel)) } },
        )
    }
}

private fun zoneCity(value: String): String = value.substringAfterLast('/').replace('_', ' ')

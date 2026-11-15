package social.waddle.android.feature.events

import android.content.Intent
import android.net.Uri
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import coil3.compose.AsyncImage
import social.waddle.android.R
import social.waddle.android.ui.huddle.HuddleCard
import social.waddle.client.ffi.WaddlePlanningItem
import social.waddle.client.ffi.WaddlePlanningKind

@Composable
internal fun PlanningCard(
    item: WaddlePlanningItem, items: List<WaddlePlanningItem>, self: String, busy: Boolean, mayEdit: Boolean, mayDelete: Boolean,
    onEdit: () -> Unit, onSave: (WaddlePlanningItem) -> Unit, onDelete: () -> Unit, onDiscuss: () -> Unit,
) {
    val context = LocalContext.current
    HuddleCard(modifier = Modifier.fillMaxWidth()) {
        Text(item.title, style = MaterialTheme.typography.titleMedium)
        item.description?.let { Text(it) }
        item.category?.let { Text(it) }
        item.location?.let { Text(it) }
        item.startsAt?.let { Text(planningDisplayTime(it, item.timezone)) }
        item.endsAt?.let { Text(planningDisplayTime(it, item.timezone)) }
        item.assignee?.let { Text(it) }
        item.status?.let { Text(it.name.lowercase()) }
        item.attendance?.let { Text("${item.author.orEmpty()}: ${it.name.lowercase()}") }
        item.amountMinor?.let { amount ->
            val formatted = runCatching {
                val currency = java.util.Currency.getInstance(item.currency ?: "GBP")
                val number = java.text.NumberFormat.getCurrencyInstance().apply { this.currency = currency }
                number.format(java.math.BigDecimal.valueOf(amount, currency.defaultFractionDigits))
            }.getOrDefault("$amount ${item.currency.orEmpty()}")
            Text(formatted)
        }
        if (item.participants.isNotEmpty()) Text(item.participants.joinToString(", "))
        val url = safePlanningUrl(item.url)
        if (url != null && item.category == "photo") {
            AsyncImage(
                model = url, contentDescription = item.title,
                modifier = Modifier.fillMaxWidth().heightIn(max = 280.dp)
            )
        }
        if (url != null) {
            TextButton(onClick = {
            runCatching { context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url))) }
        }) { Text(stringResource(R.string.events_open_link)) }
        }
        if (item.kind == WaddlePlanningKind.POLL) {
            item.options.forEach { option ->
                val votes = items.count {
                    it.kind == WaddlePlanningKind.VOTE && it.subject == item.id && option.id in it.selections
                }
                val ownVote = items.firstOrNull {
                    it.kind == WaddlePlanningKind.VOTE && it.subject == item.id && it.author == self
                }
                TextButton(enabled = !busy && !item.completed, onClick = {
                    onSave(
                        newPlanningItem(WaddlePlanningKind.VOTE, item.title).copy(
                        subject = item.id, selections = listOf(option.id),
                    )
                    )
                }) { Text("${if (option.id in ownVote?.selections.orEmpty()) "✓ " else ""}${option.label} ($votes)") }
            }
        }
        if (item.kind !in listOf(WaddlePlanningKind.VOTE, WaddlePlanningKind.ATTENDANCE)) {
            FlowRow {
                TextButton(onClick = onDiscuss) { Text(stringResource(R.string.events_discuss)) }
                if (mayEdit) {
                    TextButton(onClick = onEdit, enabled = !busy && mayEdit) {
                        Text(stringResource(R.string.events_item_edit))
                    }
                    TextButton(onClick = { onSave(item.copy(pinned = !item.pinned)) }, enabled = !busy && mayEdit) {
                        Text(stringResource(if (item.pinned) R.string.events_unpin else R.string.events_pin))
                    }
                    if (item.kind in listOf(WaddlePlanningKind.TASK, WaddlePlanningKind.POLL)) {
                        TextButton(
                            onClick = { onSave(item.copy(completed = !item.completed)) }, enabled = !busy && mayEdit,
                        ) {
                            Text(
                                stringResource(
                                if (item.completed) {
                                    R.string.events_reopen
                                } else if (item.kind == WaddlePlanningKind.POLL) {
                                    R.string.events_close_poll
                                } else {
                                    R.string.events_done
                                },
                            )
                            )
                        }
                    }
                    if (item.kind != WaddlePlanningKind.EVENT && mayDelete) {
                        TextButton(onClick = onDelete, enabled = !busy && mayEdit) {
                            Text(stringResource(R.string.events_item_delete))
                        }
                    }
                }
            }
        }
    }
}

internal fun safePlanningUrl(url: String?): String? = url?.takeIf {
    runCatching {
        val uri = java.net.URI(it)
        uri.scheme in listOf("http", "https") && !uri.host.isNullOrBlank() && uri.userInfo == null
    }.getOrDefault(false)
}

internal fun planningDisplayTime(value: Long, timezone: String?): String = runCatching {
    val zone = java.time.ZoneId.of(timezone ?: "UTC")
    java.time.Instant.ofEpochMilli(value).atZone(zone)
        .format(java.time.format.DateTimeFormatter.ofPattern("d MMM yyyy HH:mm VV"))
}.getOrElse { java.time.Instant.ofEpochMilli(value).toString() }

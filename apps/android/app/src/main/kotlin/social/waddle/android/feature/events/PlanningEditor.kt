package social.waddle.android.feature.events

import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import social.waddle.android.R
import social.waddle.client.ffi.WaddlePlanningItem
import social.waddle.client.ffi.WaddlePlanningKind
import social.waddle.client.ffi.WaddlePlanningOption
import social.waddle.client.ffi.WaddlePlanningStatus
import java.time.LocalDateTime
import java.time.ZoneId

@Composable
internal fun PlanningEditor(
    initial: WaddlePlanningItem, busy: Boolean, onCancel: () -> Unit, onSave: (WaddlePlanningItem) -> Unit,
) {
    val key = initial.id
    var title by rememberSaveable(key) { mutableStateOf(initial.title) }
    var description by rememberSaveable(key) { mutableStateOf(initial.description.orEmpty()) }
    var url by rememberSaveable(key) { mutableStateOf(initial.url.orEmpty()) }
    var category by rememberSaveable(key) { mutableStateOf(initial.category.orEmpty()) }
    var location by rememberSaveable(key) { mutableStateOf(initial.location.orEmpty()) }
    var timezone by rememberSaveable(key) { mutableStateOf(initial.timezone ?: ZoneId.systemDefault().id) }
    var start by rememberSaveable(key) { mutableStateOf(planningLocal(initial.startsAt, timezone)) }
    var end by rememberSaveable(key) { mutableStateOf(planningLocal(initial.endsAt, timezone)) }
    var assignee by rememberSaveable(key) { mutableStateOf(initial.assignee.orEmpty()) }
    var amount by rememberSaveable(key) { mutableStateOf(initial.amountMinor?.toString().orEmpty()) }
    var currency by rememberSaveable(key) { mutableStateOf(initial.currency ?: "GBP") }
    var participants by rememberSaveable(key) { mutableStateOf(initial.participants.joinToString(", ")) }
    var options by rememberSaveable(key) { mutableStateOf(initial.options.joinToString("\n") { it.label }) }
    var status by rememberSaveable(key) { mutableStateOf(initial.status?.name ?: WaddlePlanningStatus.PROPOSED.name) }
    val timed = initial.kind in listOf(
        WaddlePlanningKind.EVENT, WaddlePlanningKind.ITINERARY, WaddlePlanningKind.TRAVEL, WaddlePlanningKind.TASK,
    )
    val dated = runCatching {
        val from = planningInstant(start, timezone)
        val until = planningInstant(end, timezone)
        require(until == null || from != null && until > from)
        from to until
    }
    val amountValid = amount.toLongOrNull()?.let { it in 0..9_007_199_254_740_991L } == true
    val valid = title.isNotBlank() && title.length <= 512 && description.length <= 16_384 &&
        category.length <= 128 && location.length <= 2048 && url.length <= 8192 &&
        (!timed || dated.isSuccess) &&
        (url.isBlank() || safePlanningUrl(url) != null) &&
        (initial.kind != WaddlePlanningKind.LINK || safePlanningUrl(url) != null) &&
        (initial.kind != WaddlePlanningKind.EXPENSE || amountValid) &&
        (
            initial.kind != WaddlePlanningKind.EXPENSE ||
            runCatching { java.util.Currency.getInstance(currency) }.isSuccess
        ) &&
        (initial.kind != WaddlePlanningKind.POLL || options.lines().count { it.isNotBlank() } >= 2)
    PlanField(title, { title = it }, R.string.events_name, busy)
    PlanField(description, { description = it }, R.string.events_description, busy)
    PlanField(category, { category = it }, R.string.events_category, busy)
    if (initial.kind == WaddlePlanningKind.LINK) {
        FlowRow {
            listOf("accommodation", "transport", "restaurant", "map", "file", "photo").forEach { value ->
                TextButton(onClick = { category = value }, enabled = !busy) { Text(value) }
            }
        }
    }
    if (initial.kind in listOf(WaddlePlanningKind.LINK, WaddlePlanningKind.ITINERARY, WaddlePlanningKind.TRAVEL)) {
        PlanField(url, { url = it }, R.string.events_url, busy)
    }
    if (timed) {
        PlanField(location, { location = it }, R.string.events_location, busy)
        PlanField(timezone, { timezone = it }, R.string.events_timezone, busy)
        PlanField(start, { start = it }, R.string.events_local_start, busy)
        PlanField(end, { end = it }, R.string.events_local_end, busy)
    }
    if (initial.kind in listOf(WaddlePlanningKind.TASK, WaddlePlanningKind.ITINERARY, WaddlePlanningKind.DECISION)) {
        PlanField(assignee, { assignee = it }, R.string.events_assignee, busy)
    }
    if (initial.kind == WaddlePlanningKind.EXPENSE) {
        PlanField(amount, { amount = it }, R.string.events_amount, busy)
        PlanField(currency, { currency = it.uppercase() }, R.string.events_currency, busy)
        PlanField(participants, { participants = it }, R.string.events_participants, busy)
    }
    if (initial.kind == WaddlePlanningKind.POLL) PlanField(options, { options = it }, R.string.events_options, busy)
    if (timed || initial.kind == WaddlePlanningKind.DECISION) {
        FlowRow {
            listOf(
                WaddlePlanningStatus.PROPOSED to R.string.events_proposed,
                WaddlePlanningStatus.CONFIRMED to R.string.events_confirmed,
                WaddlePlanningStatus.CANCELLED to R.string.events_cancelled,
            ).forEach { (value, label) ->
                TextButton(onClick = { status = value.name }, enabled = !busy) {
                    Text("${if (value.name == status) "✓ " else ""}${stringResource(label)}")
                }
            }
        }
    }
    if (!valid && title.isNotEmpty()) {
        Text(stringResource(R.string.events_invalid_plan), color = MaterialTheme.colorScheme.error)
    }
    FlowRow {
        Button(enabled = valid && !busy, onClick = {
            onSave(
                initial.copy(
                title = title.trim(), description = description.trim().ifBlank { null },
                url = url.trim().ifBlank { null }, category = category.trim().ifBlank { null },
                location = location.trim().ifBlank { null }, timezone = if (timed) timezone else initial.timezone,
                startsAt = if (timed) dated.getOrThrow().first else initial.startsAt,
                endsAt = if (timed) dated.getOrThrow().second else initial.endsAt,
                assignee = assignee.trim().ifBlank { null }, amountMinor = amount.toLongOrNull(),
                currency = if (initial.kind == WaddlePlanningKind.EXPENSE) currency else initial.currency,
                participants = participants.split(',').map { it.trim() }.filter { it.isNotBlank() },
                options = if (initial.kind == WaddlePlanningKind.POLL) {
                    options.lines().filter { it.isNotBlank() }
                    .mapIndexed { index, label ->
                        WaddlePlanningOption(
                            initial.options.getOrNull(index)?.id ?: java.util.UUID.randomUUID().toString(),
                            label.trim(),
                        )
                    }
                } else {
                    initial.options
                },
                status = WaddlePlanningStatus.valueOf(status),
            )
            )
        }) { Text(stringResource(R.string.events_save)) }
        TextButton(onClick = onCancel, enabled = !busy) { Text(stringResource(R.string.events_cancel)) }
    }
}

@Composable
private fun PlanField(value: String, onChange: (String) -> Unit, label: Int, busy: Boolean) {
    OutlinedTextField(
        value, onChange, label = { Text(stringResource(label)) },
        modifier = Modifier.fillMaxWidth(), enabled = !busy
    )
}

/** Reject both sides of DST transitions rather than silently shifting an itinerary. */
internal fun planningInstant(value: String, timezone: String): Long? {
    val zone = ZoneId.of(timezone)
    if (value.isBlank()) return null
    val local = LocalDateTime.parse(value)
    val offsets = zone.rules.getValidOffsets(local)
    require(offsets.size == 1)
    return local.toInstant(offsets.single()).toEpochMilli()
}

internal fun planningLocal(value: Long?, timezone: String): String = value?.let {
    java.time.Instant.ofEpochMilli(it).atZone(ZoneId.of(timezone)).toLocalDateTime().toString()
}.orEmpty()

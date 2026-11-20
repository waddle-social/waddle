package social.waddle.android.feature.events

import android.text.format.DateFormat
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.CalendarMonth
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.Schedule
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DatePicker
import androidx.compose.material3.DatePickerDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TimePicker
import androidx.compose.material3.TimePickerLayoutType
import androidx.compose.material3.rememberDatePickerState
import androidx.compose.material3.rememberTimePickerState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import social.waddle.android.R
import social.waddle.android.theme.Waddle
import java.time.Instant
import java.time.LocalDate
import java.time.LocalDateTime
import java.time.LocalTime
import java.time.ZoneOffset
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle

/** Calendar selections use UTC midnight; event times remain local to the editor's chosen zone. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun EventDateTimeField(
    value: String,
    onChange: (String) -> Unit,
    label: String,
    enabled: Boolean,
    dateOnly: Boolean = false,
    initialPickerDate: LocalDate = LocalDate.now(),
) {
    var dialog by rememberSaveable(label, dateOnly) { mutableStateOf<String?>(null) }
    var pendingDate by rememberSaveable(label, dateOnly) { mutableStateOf<String?>(null) }
    val date = runCatching { LocalDate.parse(value.take(10)) }.getOrNull()
    val time = if (dateOnly) null else runCatching { LocalDateTime.parse(value).toLocalTime() }.getOrNull()
    val locale = LocalConfiguration.current.locales[0]
    val is24Hour = DateFormat.is24HourFormat(LocalContext.current)
    val dateDescription = stringResource(R.string.events_date_control, label)
    val timeDescription = stringResource(R.string.events_time_control, label)
    Column(verticalArrangement = Arrangement.spacedBy(Waddle.space.xxs)) {
        Text(label, style = MaterialTheme.typography.labelLarge)
        FlowRow(horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs)) {
            OutlinedButton(
                onClick = { dialog = "date" }, enabled = enabled,
                modifier = Modifier.semantics { contentDescription = dateDescription },
            ) {
                Icon(Icons.Outlined.CalendarMonth, contentDescription = null)
                Spacer(Modifier.width(Waddle.space.xs))
                Text(
                    date?.format(DateTimeFormatter.ofLocalizedDate(FormatStyle.MEDIUM).withLocale(locale))
                    ?: stringResource(R.string.events_choose_date)
                )
            }
            if (!dateOnly) {
                OutlinedButton(
                    onClick = { dialog = "time" }, enabled = enabled && date != null,
                    modifier = Modifier.semantics { contentDescription = timeDescription },
                ) {
                    Icon(Icons.Outlined.Schedule, contentDescription = null)
                    Spacer(Modifier.width(Waddle.space.xs))
                    Text(
                        time?.format(DateTimeFormatter.ofPattern(if (is24Hour) "HH:mm" else "h:mm a", locale))
                        ?: stringResource(R.string.events_choose_time)
                    )
                }
            }
            if (value.isNotEmpty()) {
                IconButton(onClick = { onChange("") }, enabled = enabled) {
                    Icon(Icons.Outlined.Close, contentDescription = stringResource(R.string.events_clear_date, label))
                }
            }
        }
    }
    if (dialog == "date") {
        val state = rememberDatePickerState(
            initialSelectedDateMillis = date?.let(::calendarPickerMillis),
            initialDisplayedMonthMillis = calendarPickerMillis(date ?: initialPickerDate),
        )
        DatePickerDialog(
            onDismissRequest = { dialog = null },
            confirmButton = {
                TextButton(enabled = enabled && state.selectedDateMillis != null, onClick = {
                    val selected = calendarPickerDate(state.selectedDateMillis!!)
                    if (dateOnly) {
                        onChange(selected.toString())
                        dialog = null
                    } else if (time != null) {
                        onChange(LocalDateTime.of(selected, time).toString())
                        dialog = null
                    } else {
                        pendingDate = selected.toString()
                        dialog = "time"
                    }
                }) { Text(stringResource(R.string.events_picker_done)) }
            },
            dismissButton = {
                TextButton(onClick = { dialog = null }) { Text(stringResource(R.string.events_cancel)) }
            },
        ) { DatePicker(state = state, showModeToggle = false) }
    }
    if (dialog == "time") {
        val state = rememberTimePickerState(
            initialHour = time?.hour ?: 9, initialMinute = time?.minute ?: 0, is24Hour = is24Hour,
        )
        AlertDialog(
            onDismissRequest = {
                dialog = null
                pendingDate = null
            },
            title = { Text(stringResource(R.string.events_choose_time)) },
            text = {
                Column(Modifier.verticalScroll(rememberScrollState())) {
                    TimePicker(state = state, layoutType = TimePickerLayoutType.Vertical)
                }
            },
            confirmButton = {
                TextButton(enabled = enabled, onClick = {
                    val selected = pendingDate?.let(LocalDate::parse) ?: date!!
                    onChange(LocalDateTime.of(selected, LocalTime.of(state.hour, state.minute)).toString())
                    dialog = null
                    pendingDate = null
                }) { Text(stringResource(R.string.events_picker_done)) }
            },
            dismissButton = {
                TextButton(onClick = {
                    dialog = null
                    pendingDate = null
                }) { Text(stringResource(R.string.events_cancel)) }
            },
        )
    }
}

internal fun calendarPickerMillis(date: LocalDate): Long = date.atStartOfDay(ZoneOffset.UTC).toInstant().toEpochMilli()

internal fun calendarPickerDate(millis: Long): LocalDate =
    Instant.ofEpochMilli(millis).atOffset(ZoneOffset.UTC).toLocalDate()

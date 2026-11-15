package social.waddle.android.feature.events

import social.waddle.client.ffi.WaddlePlanningItem
import social.waddle.client.ffi.WaddlePlanningKind
import java.time.Instant
import java.time.ZoneOffset
import java.time.format.DateTimeFormatter

/** Local export keeps private details out of the public calendar feed. */
internal fun planningCalendar(items: List<WaddlePlanningItem>, room: String): String {
    val format = DateTimeFormatter.ofPattern("yyyyMMdd'T'HHmmss'Z'").withZone(ZoneOffset.UTC)
    fun stamp(ms: Long) = format.format(Instant.ofEpochMilli(ms))
    fun escape(text: String) = text.replace("\\", "\\\\").replace("\r\n", "\n")
        .replace("\r", "\n").replace("\n", "\\n").replace(";", "\\;").replace(",", "\\,")
    val lines = mutableListOf("BEGIN:VCALENDAR", "VERSION:2.0", "PRODID:-//Waddle//Event Planning//EN")
    items.filter { it.kind in listOf(WaddlePlanningKind.EVENT, WaddlePlanningKind.ITINERARY) && it.startsAt != null }
        .forEach { item ->
            lines += listOf(
                "BEGIN:VEVENT", "UID:${escape("${item.id}@$room")}",
                "DTSTAMP:${stamp(System.currentTimeMillis())}", "DTSTART:${stamp(item.startsAt!!)}",
                "SUMMARY:${escape(item.title)}"
            )
            item.endsAt?.let { lines += "DTEND:${stamp(it)}" }
            item.description?.let { lines += "DESCRIPTION:${escape(it)}" }
            item.location?.let { lines += "LOCATION:${escape(it)}" }
            if (item.status?.name == "CANCELLED") lines += "STATUS:CANCELLED"
            lines += "END:VEVENT"
        }
    lines += "END:VCALENDAR"
    return lines.flatMap { line ->
        val folded = mutableListOf<String>()
        val current = StringBuilder()
        var bytes = 0
        line.codePoints().forEach { code ->
            val character = String(Character.toChars(code))
            val size = character.toByteArray(Charsets.UTF_8).size
            if (bytes + size > 75) {
                folded += current.toString()
                current.clear().append(' ')
                bytes = 1
            }
            current.append(character)
            bytes += size
        }
        folded += current.toString()
        folded
    }.joinToString("\r\n", postfix = "\r\n")
}

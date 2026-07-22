package social.waddle.android.feature.profile

import social.waddle.client.ffi.WaddleActivity
import social.waddle.client.ffi.WaddleMood
import social.waddle.client.ffi.WaddleTune
import java.net.URI

/**
 * Pure ports of the web client's `status-publication-ui.ts` draft
 * builders: each turns raw field text into a typed FFI publication or
 * a set of field errors, with identical normalization rules.
 */

private val SNAKE_CASE = Regex("^[a-z0-9]+(_[a-z0-9]+)*$")

private fun trimOptional(value: String): String? = value.trim().ifEmpty { null }

/** `in_awe` → `In Awe` (display labels for the closed vocabularies). */
fun formatPepKeyword(value: String): String =
    value.split('_').joinToString(separator = " ") { segment ->
        segment.replaceFirstChar { it.uppercaseChar() }
    }

/** Free-text specific activity → publishable snake_case identifier. */
fun normalizeActivitySpecific(value: String): String =
    value.trim()
        .lowercase()
        .replace(Regex("['’]"), "")
        .replace(Regex("[^a-z0-9]+"), "_")
        .trim('_')

data class MoodDraft(val kind: String = "", val text: String = "")

/** `null` when no mood kind is selected (the only invalid shape). */
fun buildMoodPublication(draft: MoodDraft): WaddleMood? {
    if (draft.kind.isEmpty()) return null
    return WaddleMood(kind = draft.kind, text = trimOptional(draft.text))
}

data class ActivityDraft(
    val general: String = "",
    val specific: String = "",
    val text: String = "",
)

enum class ActivityFieldError { MISSING_GENERAL, INVALID_SPECIFIC }

data class ActivityBuild(
    val publication: WaddleActivity?,
    val errors: Set<ActivityFieldError>,
)

fun buildActivityPublication(draft: ActivityDraft): ActivityBuild {
    val errors = mutableSetOf<ActivityFieldError>()
    if (draft.general.isEmpty()) errors += ActivityFieldError.MISSING_GENERAL
    val specific = normalizeActivitySpecific(draft.specific)
    val hadSpecificInput = draft.specific.isNotBlank()
    if (hadSpecificInput && (specific.isEmpty() || !SNAKE_CASE.matches(specific))) {
        errors += ActivityFieldError.INVALID_SPECIFIC
    }
    if (errors.isNotEmpty()) return ActivityBuild(publication = null, errors = errors)
    return ActivityBuild(
        publication = WaddleActivity(
            general = draft.general,
            specific = specific.ifEmpty { null },
            text = trimOptional(draft.text),
        ),
        errors = emptySet(),
    )
}

data class TuneDraft(
    val artist: String = "",
    val title: String = "",
    val source: String = "",
    val length: String = "",
    val rating: String = "",
    val track: String = "",
    val uri: String = "",
)

enum class TuneFieldError { EMPTY_FORM, INVALID_LENGTH, INVALID_RATING, INVALID_URI }

data class TuneBuild(
    val publication: WaddleTune?,
    val errors: Set<TuneFieldError>,
)

fun buildTunePublication(draft: TuneDraft): TuneBuild {
    val errors = mutableSetOf<TuneFieldError>()
    val length = parseBounded(draft.length, min = 1, max = Int.MAX_VALUE)
        .also { if (it == ParseOutcome.Invalid) errors += TuneFieldError.INVALID_LENGTH }
    val rating = parseBounded(draft.rating, min = 1, max = 10)
        .also { if (it == ParseOutcome.Invalid) errors += TuneFieldError.INVALID_RATING }
    val uriValue = draft.uri.trim()
    if (uriValue.isNotEmpty() && !isAbsoluteUri(uriValue)) errors += TuneFieldError.INVALID_URI

    val publication = WaddleTune(
        artist = trimOptional(draft.artist),
        title = trimOptional(draft.title),
        source = trimOptional(draft.source),
        track = trimOptional(draft.track),
        uri = if (uriValue.isNotEmpty() && TuneFieldError.INVALID_URI !in errors) uriValue else null,
        lengthSeconds = (length as? ParseOutcome.Value)?.value?.toUInt(),
        rating = (rating as? ParseOutcome.Value)?.value?.toUByte(),
    )
    val hasDetails = listOf(
        publication.artist,
        publication.title,
        publication.source,
        publication.track,
        publication.uri,
        publication.lengthSeconds,
        publication.rating,
    ).any { it != null }
    if (!hasDetails) errors += TuneFieldError.EMPTY_FORM
    if (errors.isNotEmpty()) return TuneBuild(publication = null, errors = errors)
    return TuneBuild(publication = publication, errors = emptySet())
}

private sealed interface ParseOutcome {
    data object Absent : ParseOutcome
    data object Invalid : ParseOutcome
    data class Value(val value: Int) : ParseOutcome
}

private fun parseBounded(raw: String, min: Int, max: Int): ParseOutcome {
    val trimmed = raw.trim()
    if (trimmed.isEmpty()) return ParseOutcome.Absent
    if (!trimmed.all { it.isDigit() }) return ParseOutcome.Invalid
    val parsed = trimmed.toIntOrNull() ?: return ParseOutcome.Invalid
    if (parsed < min || parsed > max) return ParseOutcome.Invalid
    return ParseOutcome.Value(parsed)
}

/** Web parity: any URI with a real scheme (`https://…`, `spotify:…`);
 *  single-letter schemes are rejected like the web's drive-path guard. */
private fun isAbsoluteUri(value: String): Boolean {
    val scheme = runCatching { URI(value).scheme }.getOrNull() ?: return false
    return scheme.length > 1
}

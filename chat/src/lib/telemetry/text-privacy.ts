const FULL_COMMIT_SHA_PATTERN = /^[0-9a-f]{40}$/i;

/** Sanitize a known telemetry string without treating arbitrary text as safe. */
export function sanitizeTelemetryText(value: string): string {
  return stripQueriesAndFragmentsFromUrls(value)
    .replace(
      /(["']?authorization["']?\s*[:=]\s*["']?)(?:(?:bearer|basic|digest)\s+)?[^"'\s,;}]+/gi,
      "$1:redacted",
    )
    .replace(/\b(bearer|basic)\s+[A-Z0-9._~+/=-]+/gi, "$1 :redacted")
    .replace(
      /(["']?(?:code|state|access_token|refresh_token|redirect_uri|id_token|client_secret|api_key|waddle_session_id|session_id)["']?\s*[:=]\s*["']?)[^"'&\s,;}]+/gi,
      "$1:redacted",
    )
    .replace(/\/api\/upload\/[^\s?#)]+/g, "/api/upload/:slot")
    .replace(/\/api\/files\/[^\s?#)]+(?:\/[^\s?#)]+)?/g, "/api/files/:slot/:file")
    .replace(/[A-Z0-9._%+-]+@[A-Z0-9.-]+(?:\/[^\s,;)]*)?/gi, ":jid");
}

export function fullCommitShaForTelemetry(value: string | undefined): string | undefined {
  if (!value) return undefined;
  const trimmed = value.trim();
  return FULL_COMMIT_SHA_PATTERN.test(trimmed) ? trimmed.toLowerCase() : undefined;
}

function stripQueriesAndFragmentsFromUrls(value: string): string {
  const absoluteUrlsStripped = value.replace(
    /\bhttps?:\/\/[^\s<>"']+/gi,
    (candidate) => stripUrlQueryAndFragment(candidate),
  );
  return absoluteUrlsStripped.replace(
    /((?:file:\/\/|\/)[^\s?#<>"']+)[?#][^\s<>"']*/gi,
    "$1",
  );
}

function stripUrlQueryAndFragment(candidate: string): string {
  const trailing = candidate.match(/[),.;:]+$/)?.[0] ?? "";
  const urlText = trailing ? candidate.slice(0, -trailing.length) : candidate;
  try {
    const url = new URL(urlText);
    url.search = "";
    url.hash = "";
    return `${url.toString()}${trailing}`;
  } catch {
    return `${urlText.split(/[?#]/, 1)[0] ?? ":redacted"}${trailing}`;
  }
}

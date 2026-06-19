export interface CalendarFeedRequestInput {
  communityJid: string | null;
  serverBaseUrl: string;
  sessionId?: string | null;
}

export type CalendarFeedFetch = (
  input: RequestInfo | URL,
  init?: RequestInit,
) => Promise<Response>;

export type CalendarFeedCopyText = (text: string) => Promise<void>;

export type CalendarFeedCopyResult =
  | { status: "copied"; url: string }
  | { status: "copy_failed"; url: string }
  | { status: "request_failed" };

export interface CalendarFeedCopyView {
  state: "idle" | "loading" | "copied" | "error";
  url: string | null;
}

function isLoopbackHost(hostname: string): boolean {
  return hostname === "localhost"
    || hostname === "127.0.0.1"
    || hostname === "::1";
}

export function isAllowedCalendarFeedUrl(value: string): boolean {
  try {
    const url = new URL(value);
    if (url.protocol === "https:" || url.protocol === "webcal:") return true;
    return url.protocol === "http:" && isLoopbackHost(url.hostname);
  } catch {
    return false;
  }
}

export function calendarFeedSubscriptionHref(value: string): string | null {
  if (!isAllowedCalendarFeedUrl(value)) return null;
  const url = new URL(value);
  if (url.protocol === "webcal:") return url.toString();
  return `webcal:${url.toString().slice(url.protocol.length)}`;
}

export function nextCalendarFeedCopyViewState(input: CalendarFeedCopyView & {
  contextChanged?: boolean;
  startAttempt?: boolean;
  result?: CalendarFeedCopyResult;
}): CalendarFeedCopyView {
  if (input.contextChanged) {
    return { state: "idle", url: null };
  }
  if (input.startAttempt) {
    return { state: "loading", url: null };
  }
  if (!input.result) {
    return { state: input.state, url: input.url };
  }
  switch (input.result.status) {
    case "copied":
      return { state: "copied", url: input.result.url };
    case "copy_failed":
      return { state: "error", url: input.result.url };
    case "request_failed":
      return { state: "error", url: null };
  }
}

export function calendarFeedEndpoint(input: CalendarFeedRequestInput): string | null {
  if (!input.communityJid) return null;
  try {
    const url = new URL("/api/calendar/community-feed-url", input.serverBaseUrl);
    url.searchParams.set("community_jid", input.communityJid);
    return url.toString();
  } catch {
    return null;
  }
}

export function isSameCalendarFeedRequestInput(
  a: CalendarFeedRequestInput,
  b: CalendarFeedRequestInput,
): boolean {
  return a.communityJid === b.communityJid
    && a.serverBaseUrl === b.serverBaseUrl
    && (a.sessionId ?? null) === (b.sessionId ?? null);
}

async function requestCalendarFeedUrl(
  input: CalendarFeedRequestInput,
  fetchImpl: CalendarFeedFetch,
): Promise<string> {
  const endpoint = calendarFeedEndpoint(input);
  if (!endpoint) throw new Error("calendar feed endpoint unavailable");

  const headers: Record<string, string> = { "Accept": "application/json" };
  if (input.sessionId) {
    headers["X-Waddle-Session-Id"] = input.sessionId;
  }
  const response = await fetchImpl(endpoint, {
    credentials: "include",
    headers,
  });
  if (!response.ok) throw new Error(`calendar feed request failed: ${response.status}`);

  const body = await response.json() as { url?: unknown };
  if (
    typeof body.url !== "string"
    || body.url.length === 0
    || !isAllowedCalendarFeedUrl(body.url)
  ) {
    throw new Error("calendar feed response missing url");
  }
  return body.url;
}

async function copyTextToClipboard(text: string): Promise<void> {
  if (typeof navigator !== "undefined" && navigator.clipboard?.writeText) {
    await navigator.clipboard.writeText(text);
    return;
  }
  throw new Error("clipboard unavailable");
}

export async function copyCalendarFeedUrlToClipboard(
  input: CalendarFeedRequestInput,
  deps: {
    fetch?: CalendarFeedFetch;
    copyText?: CalendarFeedCopyText;
  } = {},
): Promise<CalendarFeedCopyResult> {
  let url: string;
  try {
    const fetchImpl = deps.fetch ?? (typeof fetch !== "undefined" ? fetch : undefined);
    if (!fetchImpl) return { status: "request_failed" };
    url = await requestCalendarFeedUrl(input, fetchImpl);
  } catch {
    return { status: "request_failed" };
  }

  try {
    await (deps.copyText ?? copyTextToClipboard)(url);
    return { status: "copied", url };
  } catch {
    return { status: "copy_failed", url };
  }
}

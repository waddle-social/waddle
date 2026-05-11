// Pure helpers for XEP-0313 Message Archive Management paging.
//
// These are deliberately Vue-free and stanza-free. The composables that wrap
// `BrowserXmppClient` are responsible for issuing requests and committing the
// returned cursor/page into their reactive state; this module only encodes the
// data shaping that's repeated across every MAM consumer (channels +
// channel-threads + DMs + search). See XEP-0313 §4 "Querying an archive" and
// §4.3 "Paging through results"; the §4.3.4 item-not-found gotcha is handled
// via the dedicated error type below.

type CursorState = Readonly<{
  oldestArchiveId: string | null;
  hasOlderMessages: boolean;
}>;

export const INITIAL_CURSOR: CursorState = Object.freeze({
  oldestArchiveId: null,
  hasOlderMessages: true,
});

type RsmPage = {
  firstArchiveId?: string;
  complete?: boolean;
};

type RsmPageWithMessages = RsmPage & {
  messages: ReadonlyArray<unknown>;
};

type CursorAdvanceResult = Readonly<{
  next: CursorState;
  stalled: boolean;
}>;

export function cursorFromLatestPage(opts: {
  page: RsmPageWithMessages | null;
  pageSize: number;
  fallbackMessages?: ReadonlyArray<{ id?: string }>;
}): CursorState {
  const { page, pageSize, fallbackMessages } = opts;
  if (page) {
    const firstArchiveId = page.firstArchiveId ?? null;
    const hasOlder = !page.complete && firstArchiveId !== null;
    return { oldestArchiveId: firstArchiveId, hasOlderMessages: hasOlder };
  }
  if (!fallbackMessages || fallbackMessages.length === 0) {
    return { oldestArchiveId: null, hasOlderMessages: false };
  }
  const oldest = fallbackMessages[0]?.id ?? null;
  return {
    oldestArchiveId: oldest,
    hasOlderMessages: fallbackMessages.length >= pageSize,
  };
}

export function advanceCursorWithBeforePage(opts: {
  prior: CursorState;
  page: RsmPage;
}): CursorAdvanceResult {
  const { prior, page } = opts;
  const nextOldest = page.firstArchiveId ?? prior.oldestArchiveId;
  const stalled =
    !page.firstArchiveId ||
    page.firstArchiveId === prior.oldestArchiveId;
  const hasOlder = !page.complete && !!page.firstArchiveId && !stalled;
  return {
    next: { oldestArchiveId: nextOldest, hasOlderMessages: hasOlder },
    stalled,
  };
}

type ThreadCursorEntry = Readonly<{
  oldestId: string | undefined;
  hasOlder: boolean;
}>;

export function threadCursorFromLatestPage(opts: {
  page: RsmPageWithMessages | null;
  pageSize: number;
  fallbackMessages?: ReadonlyArray<unknown>;
}): ThreadCursorEntry {
  const { page, pageSize, fallbackMessages } = opts;
  if (page) {
    return {
      oldestId: page.firstArchiveId,
      hasOlder: !page.complete && !!page.firstArchiveId,
    };
  }
  const count = fallbackMessages?.length ?? 0;
  return {
    oldestId: undefined,
    hasOlder: count >= pageSize,
  };
}

export function advanceThreadCursor(opts: {
  prior: { oldestId?: string };
  page: RsmPage;
}): ThreadCursorEntry {
  const { prior, page } = opts;
  const stalled =
    !page.firstArchiveId || page.firstArchiveId === prior.oldestId;
  return {
    oldestId: page.firstArchiveId ?? prior.oldestId,
    hasOlder: !page.complete && !!page.firstArchiveId && !stalled,
  };
}

export function stripQueuedSelfMessages<
  M extends { isSelf?: boolean; deliveryStatus?: string },
>(timeline: ReadonlyArray<M>): M[] {
  return timeline.filter((m) => !(m.isSelf && m.deliveryStatus === "queued"));
}

// XEP-0313 §4.3.4: the server MUST return <item-not-found/> when the UID
// referenced by <before/>/<after/> doesn't exist in the archive. Treating
// that as "no more history" is incorrect — the right move is to drop the
// stale cursor and re-fetch the tail page. Callers detect this via the
// dedicated error type below, which `classifyMamError` lifts out of the
// various raw error shapes the XMPP client may surface.
// Sentinel used when the cursor value can't be extracted from the raw error
// shape (current waddle wasm client surfaces errors as plain strings without
// the offending UID). Telemetry/log readers should treat this as "unknown
// cursor, classified by substring match" rather than an empty cursor.
export const MAM_UNKNOWN_CURSOR = "<unknown>";

export class MamCursorNotFoundError extends Error {
  readonly cursor: string;
  constructor(cursor: string) {
    super(`MAM cursor "${cursor}" not found in archive (XEP-0313 §4.3.4)`);
    this.name = "MamCursorNotFoundError";
    this.cursor = cursor;
  }
}

export function isMamCursorNotFound(value: unknown): value is MamCursorNotFoundError {
  return value instanceof MamCursorNotFoundError;
}

export function classifyMamError(error: unknown): MamCursorNotFoundError | unknown {
  // String-form fallback: the current wasm client surfaces errors as plain
  // strings via `JsValue::from_str(err.to_string())` (see
  // server/crates/waddle-xmpp-client-wasm/src/helpers.rs). Until that layer
  // emits structured errors, the only way to detect a §4.3.4 condition is
  // a substring match on the message. Acceptable as long as the underlying
  // stanza-error condition name appears literally somewhere in the string.
  if (typeof error === "string") {
    return error.includes("item-not-found")
      ? new MamCursorNotFoundError(MAM_UNKNOWN_CURSOR)
      : error;
  }
  if (error instanceof Error && error.message.includes("item-not-found")) {
    return new MamCursorNotFoundError(MAM_UNKNOWN_CURSOR);
  }
  if (!error || typeof error !== "object") return error;
  const candidate = error as { condition?: unknown; code?: unknown; cursor?: unknown };
  const condition = typeof candidate.condition === "string" ? candidate.condition : undefined;
  const code = typeof candidate.code === "string" ? candidate.code : undefined;
  const cursor = typeof candidate.cursor === "string" ? candidate.cursor : "";
  if (condition === "item-not-found" || code === "item-not-found") {
    return new MamCursorNotFoundError(cursor);
  }
  return error;
}

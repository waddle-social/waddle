import { reportError } from "@/lib/telemetry";

/**
 * Client for the server's `/api/bots` management routes.
 *
 * Bot management is an HTTP API, not XMPP: bots and their tokens are
 * account administration, like the session and calendar feed helpers.
 * Responses map snake_case JSON to camelCase and every failure resolves to
 * a typed `BotsResult` instead of throwing. Token secrets and JIDs never
 * reach telemetry; only the operation and HTTP status do.
 */

export type BotTokenSummary = {
  id: string;
  label: string;
  createdAt: string;
  lastUsedAt: string | null;
  expiresAt: string;
  revokedAt: string | null;
};

type BotDisabledReason = "owner_missing" | "owner_not_member";

export type BotSummary = {
  jid: string;
  name: string;
  displayName: string | null;
  createdAt: string;
  disabledAt: string | null;
  disabledReason: BotDisabledReason | null;
  tokens: BotTokenSummary[];
};

export type BotsRequestContext = {
  serverBaseUrl: string;
  sessionId: string | null;
  fetchImpl?: typeof fetch;
};

type BotsConflictCode =
  | "bot_exists"
  | "bot_limit_reached"
  | "bot_disabled"
  | "bot_token_limit_reached";

export type BotsResult<T> =
  | { kind: "ok"; value: T }
  | { kind: "unauthorized" }
  | { kind: "forbidden"; code: string }
  | { kind: "conflict"; code: BotsConflictCode }
  | { kind: "invalid"; code: string }
  | { kind: "not-found" }
  | { kind: "failed"; status: number | null };

type BotsOperation =
  | "bots-list-get"
  | "bots-create-post"
  | "bots-delete"
  | "bot-token-mint-post"
  | "bot-token-revoke-delete";

/** Mirrors the server's `BotName` rule. */
export const BOT_NAME_PATTERN = /^[a-z0-9](?:[a-z0-9_-]{0,30}[a-z0-9])?$/;

const CONFLICT_CODES: readonly BotsConflictCode[] = [
  "bot_exists",
  "bot_limit_reached",
  "bot_disabled",
  "bot_token_limit_reached",
];

const DISABLED_REASONS: readonly BotDisabledReason[] = ["owner_missing", "owner_not_member"];

function botsEndpoint(serverBaseUrl: string, path: string): string | null {
  const base = serverBaseUrl.trim();
  if (!base) return path;
  try {
    return new URL(path, base).toString();
  } catch {
    return null;
  }
}

function botPath(jid: string): string {
  return `/api/bots/${encodeURIComponent(jid)}`;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isNullableString(value: unknown): value is string | null {
  return value === null || typeof value === "string";
}

function parseToken(value: unknown): BotTokenSummary | null {
  if (!isRecord(value)) return null;
  const { id, label, created_at, last_used_at, expires_at, revoked_at } = value;
  if (
    typeof id !== "string"
    || typeof label !== "string"
    || typeof created_at !== "string"
    || !isNullableString(last_used_at)
    || typeof expires_at !== "string"
    || !isNullableString(revoked_at)
  ) {
    return null;
  }
  return {
    id,
    label,
    createdAt: created_at,
    lastUsedAt: last_used_at,
    expiresAt: expires_at,
    revokedAt: revoked_at,
  };
}

function parseBot(value: unknown): BotSummary | null {
  if (!isRecord(value)) return null;
  const { jid, name, display_name, created_at, disabled_at, disabled_reason, tokens } = value;
  if (
    typeof jid !== "string"
    || typeof name !== "string"
    || !isNullableString(display_name)
    || typeof created_at !== "string"
    || !isNullableString(disabled_at)
    || !Array.isArray(tokens)
  ) {
    return null;
  }
  let disabledReason: BotDisabledReason | null = null;
  if (disabled_reason !== null) {
    const known = DISABLED_REASONS.find((reason) => reason === disabled_reason);
    if (!known) return null;
    disabledReason = known;
  }
  const parsedTokens: BotTokenSummary[] = [];
  for (const token of tokens) {
    const parsed = parseToken(token);
    if (!parsed) return null;
    parsedTokens.push(parsed);
  }
  return {
    jid,
    name,
    displayName: display_name,
    createdAt: created_at,
    disabledAt: disabled_at,
    disabledReason,
    tokens: parsedTokens,
  };
}

function parseBotList(
  value: unknown,
): { bots: BotSummary[]; limit: number; tokenLimit: number } | null {
  if (!isRecord(value) || !Array.isArray(value.bots)) return null;
  const { limit, token_limit } = value;
  if (
    typeof limit !== "number"
    || !Number.isInteger(limit)
    || typeof token_limit !== "number"
    || !Number.isInteger(token_limit)
  ) {
    return null;
  }
  const bots: BotSummary[] = [];
  for (const bot of value.bots) {
    const parsed = parseBot(bot);
    if (!parsed) return null;
    bots.push(parsed);
  }
  return { bots, limit, tokenLimit: token_limit };
}

function parseMinted(value: unknown): { token: BotTokenSummary; secret: string } | null {
  if (!isRecord(value) || typeof value.secret !== "string" || !value.secret) return null;
  const token = parseToken(value.token);
  return token ? { token, secret: value.secret } : null;
}

async function errorCode(response: Response): Promise<string | null> {
  try {
    const body: unknown = await response.json();
    return isRecord(body) && typeof body.error === "string" ? body.error : null;
  } catch {
    return null;
  }
}

function failed<T>(
  operation: BotsOperation,
  status: number | null,
  recoverable: boolean,
): BotsResult<T> {
  reportError({ kind: "http.fetch", operation, status: status ?? 0, recoverable });
  return { kind: "failed", status };
}

async function errorResult<T>(
  operation: BotsOperation,
  response: Response,
): Promise<BotsResult<T>> {
  const { status } = response;
  if (status === 401) return { kind: "unauthorized" };
  if (status === 404) return { kind: "not-found" };
  if (status === 403) return { kind: "forbidden", code: (await errorCode(response)) ?? "forbidden" };
  if (status === 400) return { kind: "invalid", code: (await errorCode(response)) ?? "invalid_request" };
  if (status === 409) {
    const code = await errorCode(response);
    const known = CONFLICT_CODES.find((candidate) => candidate === code);
    if (known) return { kind: "conflict", code: known };
  }
  return failed(operation, status, status >= 500);
}

type Sent = { kind: "sent"; response: Response };

async function send(
  ctx: BotsRequestContext,
  operation: BotsOperation,
  path: string,
  init: { method: string; body?: unknown },
): Promise<Sent | BotsResult<never>> {
  const endpoint = botsEndpoint(ctx.serverBaseUrl, path);
  const fetchImpl = ctx.fetchImpl ?? (typeof fetch !== "undefined" ? fetch : undefined);
  if (!endpoint || !fetchImpl) return failed(operation, null, false);

  const headers: Record<string, string> = { "Accept": "application/json" };
  if (init.body !== undefined) headers["Content-Type"] = "application/json";
  if (ctx.sessionId) headers["X-Waddle-Session-Id"] = ctx.sessionId;

  let response: Response;
  try {
    response = await fetchImpl(endpoint, {
      method: init.method,
      credentials: "include",
      headers,
      body: init.body === undefined ? undefined : JSON.stringify(init.body),
    });
  } catch {
    return failed(operation, null, true);
  }
  if (!response.ok) return errorResult(operation, response);
  return { kind: "sent", response };
}

async function requestJson<T>(
  ctx: BotsRequestContext,
  operation: BotsOperation,
  path: string,
  init: { method: string; body?: unknown },
  parse: (body: unknown) => T | null,
): Promise<BotsResult<T>> {
  const sent = await send(ctx, operation, path, init);
  if (sent.kind !== "sent") return sent;
  let value: T | null = null;
  try {
    value = parse(await sent.response.json());
  } catch {
    value = null;
  }
  return value === null
    ? failed(operation, sent.response.status, false)
    : { kind: "ok", value };
}

async function requestNoContent(
  ctx: BotsRequestContext,
  operation: BotsOperation,
  path: string,
): Promise<BotsResult<null>> {
  const sent = await send(ctx, operation, path, { method: "DELETE" });
  return sent.kind === "sent" ? { kind: "ok", value: null } : sent;
}

export function listBots(
  ctx: BotsRequestContext,
): Promise<BotsResult<{ bots: BotSummary[]; limit: number; tokenLimit: number }>> {
  return requestJson(ctx, "bots-list-get", "/api/bots", { method: "GET" }, parseBotList);
}

export function createBot(
  ctx: BotsRequestContext,
  input: { name: string; displayName?: string },
): Promise<BotsResult<BotSummary>> {
  const displayName = input.displayName?.trim();
  const body: Record<string, string> = { name: input.name };
  if (displayName) body.display_name = displayName;
  return requestJson(ctx, "bots-create-post", "/api/bots", { method: "POST", body }, parseBot);
}

export function deleteBot(ctx: BotsRequestContext, jid: string): Promise<BotsResult<null>> {
  return requestNoContent(ctx, "bots-delete", botPath(jid));
}

export function mintBotToken(
  ctx: BotsRequestContext,
  jid: string,
  input: { label: string; expiresInDays?: number },
): Promise<BotsResult<{ token: BotTokenSummary; secret: string }>> {
  const body: Record<string, string | number> = { label: input.label };
  if (input.expiresInDays !== undefined) body.expires_in_days = input.expiresInDays;
  return requestJson(
    ctx,
    "bot-token-mint-post",
    `${botPath(jid)}/tokens`,
    { method: "POST", body },
    parseMinted,
  );
}

export function revokeBotToken(
  ctx: BotsRequestContext,
  jid: string,
  tokenId: string,
): Promise<BotsResult<null>> {
  return requestNoContent(
    ctx,
    "bot-token-revoke-delete",
    `${botPath(jid)}/tokens/${encodeURIComponent(tokenId)}`,
  );
}

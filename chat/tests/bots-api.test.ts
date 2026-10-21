import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import {
  BOT_NAME_PATTERN,
  createBot,
  deleteBot,
  listBots,
  mintBotToken,
  revokeBotToken,
  type BotsRequestContext,
} from "../src/lib/bots-api";
import { __setFaroForTesting } from "../src/lib/telemetry";

type Call = { url: string; init: RequestInit };

const BOT_JID = "rawkode+standup@bots.example.com";
const SECRET = "wbt_c2VjcmV0LXRva2VuLXZhbHVlLXRoYXQtaXMtbG9uZy1lbm91Z2g";

const tokenBody = {
  id: "0b7e9c1e-1f43-4e2b-9a55-1d6f3f0a2b10",
  label: "laptop",
  created_at: "2026-10-20T10:00:00.000000Z",
  last_used_at: null,
  expires_at: "2027-01-18T10:00:00.000000Z",
  revoked_at: null,
};

const botBody = {
  jid: BOT_JID,
  name: "standup",
  display_name: "Standup Bot",
  owner_jid: "rawkode@example.com",
  created_at: "2026-10-20T09:00:00.000000Z",
  disabled_at: null,
  disabled_reason: null,
  tokens: [tokenBody],
};

function json(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function recordingFetch(respond: (call: Call) => Response | Promise<Response>) {
  const calls: Call[] = [];
  const fetchImpl = mock(async (input: RequestInfo | URL, init?: RequestInit) => {
    const call = { url: String(input), init: init ?? {} };
    calls.push(call);
    return respond(call);
  });
  return { calls, fetchImpl: fetchImpl as unknown as typeof fetch };
}

function ctx(fetchImpl: typeof fetch, overrides: Partial<BotsRequestContext> = {}): BotsRequestContext {
  return {
    serverBaseUrl: "https://server.example.com",
    sessionId: "session-1",
    fetchImpl,
    ...overrides,
  };
}

function headersOf(call: Call): Record<string, string> {
  return call.init.headers as Record<string, string>;
}

function createFaroStub() {
  const errors: Array<{ error: Error; options?: { type?: string; context?: Record<string, string> } }> = [];
  return {
    errors,
    api: {
      pushEvent: () => undefined,
      pushMeasurement: () => undefined,
      pushError: (error: Error, options?: { type?: string; context?: Record<string, string> }) => {
        errors.push({ error, options });
      },
    },
  };
}

beforeEach(() => {
  __setFaroForTesting(null);
});

afterEach(() => {
  __setFaroForTesting(null);
});

describe("bots api", () => {
  test("builds endpoint from base url and same-origin fallback", async () => {
    const { calls, fetchImpl } = recordingFetch(() => json(200, { bots: [], limit: 10, token_limit: 10 }));

    await listBots(ctx(fetchImpl));
    await listBots(ctx(fetchImpl, { serverBaseUrl: "" }));
    await listBots(ctx(fetchImpl, { serverBaseUrl: "   " }));

    expect(calls.map((call) => call.url)).toEqual([
      "https://server.example.com/api/bots",
      "/api/bots",
      "/api/bots",
    ]);
  });

  test("encodes jid with plus and at", async () => {
    const { calls, fetchImpl } = recordingFetch(() => new Response(null, { status: 204 }));

    await deleteBot(ctx(fetchImpl), BOT_JID);
    await revokeBotToken(ctx(fetchImpl), BOT_JID, "token/1");

    expect(calls[0].url).toBe("https://server.example.com/api/bots/rawkode%2Bstandup%40bots.example.com");
    expect(calls[0].init.method).toBe("DELETE");
    expect(calls[1].url).toBe(
      "https://server.example.com/api/bots/rawkode%2Bstandup%40bots.example.com/tokens/token%2F1",
    );
  });

  test("sends session header and credentials include on every call", async () => {
    const { calls, fetchImpl } = recordingFetch((call) => {
      if (call.init.method === "DELETE") return new Response(null, { status: 204 });
      if (call.url.endsWith("/tokens")) return json(201, { token: tokenBody, secret: SECRET });
      if (call.init.method === "POST") return json(201, botBody);
      return json(200, { bots: [botBody], limit: 10, token_limit: 10 });
    });
    const context = ctx(fetchImpl);

    await listBots(context);
    await createBot(context, { name: "standup" });
    await deleteBot(context, BOT_JID);
    await mintBotToken(context, BOT_JID, { label: "laptop" });
    await revokeBotToken(context, BOT_JID, tokenBody.id);

    expect(calls).toHaveLength(5);
    for (const call of calls) {
      expect(call.init.credentials).toBe("include");
      expect(headersOf(call)["Accept"]).toBe("application/json");
      expect(headersOf(call)["X-Waddle-Session-Id"]).toBe("session-1");
      if (call.init.body === undefined) {
        expect(headersOf(call)["Content-Type"]).toBeUndefined();
      } else {
        expect(headersOf(call)["Content-Type"]).toBe("application/json");
      }
    }

    calls.length = 0;
    await listBots(ctx(fetchImpl, { sessionId: null }));
    expect(headersOf(calls[0])["X-Waddle-Session-Id"]).toBeUndefined();
    expect(calls[0].init.credentials).toBe("include");
  });

  test("create posts json body", async () => {
    const { calls, fetchImpl } = recordingFetch(() => json(201, botBody));

    const result = await createBot(ctx(fetchImpl), { name: "standup", displayName: "  Standup Bot  " });
    await createBot(ctx(fetchImpl), { name: "standup", displayName: "   " });

    expect(calls[0].init.method).toBe("POST");
    expect(calls[0].url).toBe("https://server.example.com/api/bots");
    expect(JSON.parse(String(calls[0].init.body))).toEqual({ name: "standup", display_name: "Standup Bot" });
    expect(JSON.parse(String(calls[1].init.body))).toEqual({ name: "standup" });
    expect(result).toEqual({
      kind: "ok",
      value: {
        jid: BOT_JID,
        name: "standup",
        displayName: "Standup Bot",
        createdAt: "2026-10-20T09:00:00.000000Z",
        disabledAt: null,
        disabledReason: null,
        tokens: [{
          id: tokenBody.id,
          label: "laptop",
          createdAt: tokenBody.created_at,
          lastUsedAt: null,
          expiresAt: tokenBody.expires_at,
          revokedAt: null,
        }],
      },
    });
  });

  test("list maps bots, limit and token limit", async () => {
    const disabled = {
      ...botBody,
      disabled_at: "2026-10-21T00:00:00.000000Z",
      disabled_reason: "owner_not_member",
      tokens: [],
    };
    const { fetchImpl } = recordingFetch(() => json(200, { bots: [disabled], limit: 5, token_limit: 10 }));

    const result = await listBots(ctx(fetchImpl));

    expect(result.kind).toBe("ok");
    if (result.kind !== "ok") return;
    expect(result.value.limit).toBe(5);
    expect(result.value.tokenLimit).toBe(10);
    expect(result.value.bots[0].disabledAt).toBe("2026-10-21T00:00:00.000000Z");
    expect(result.value.bots[0].disabledReason).toBe("owner_not_member");
  });

  test("mint maps secret and token", async () => {
    const { calls, fetchImpl } = recordingFetch(() => json(201, { token: tokenBody, secret: SECRET }));

    const result = await mintBotToken(ctx(fetchImpl), BOT_JID, { label: "laptop", expiresInDays: 30 });
    await mintBotToken(ctx(fetchImpl), BOT_JID, { label: "ci" });

    expect(calls[0].url).toBe(
      "https://server.example.com/api/bots/rawkode%2Bstandup%40bots.example.com/tokens",
    );
    expect(calls[0].init.method).toBe("POST");
    expect(JSON.parse(String(calls[0].init.body))).toEqual({ label: "laptop", expires_in_days: 30 });
    expect(JSON.parse(String(calls[1].init.body))).toEqual({ label: "ci" });
    expect(result).toEqual({
      kind: "ok",
      value: {
        secret: SECRET,
        token: {
          id: tokenBody.id,
          label: "laptop",
          createdAt: tokenBody.created_at,
          lastUsedAt: null,
          expiresAt: tokenBody.expires_at,
          revokedAt: null,
        },
      },
    });
  });

  test("maps 401/403/404/409 codes and 5xx to results", async () => {
    const cases: Array<[Response, unknown]> = [
      [json(401, { error: "invalid_session", message: "" }), { kind: "unauthorized" }],
      [json(403, { error: "forbidden", message: "" }), { kind: "forbidden", code: "forbidden" }],
      [json(403, { error: "owner_ineligible", message: "" }), { kind: "forbidden", code: "owner_ineligible" }],
      [json(403, { error: "untrusted_origin", message: "" }), { kind: "forbidden", code: "untrusted_origin" }],
      [json(404, { error: "not_found", message: "" }), { kind: "not-found" }],
      [json(400, { error: "invalid_name", message: "" }), { kind: "invalid", code: "invalid_name" }],
      [json(409, { error: "bot_exists", message: "" }), { kind: "conflict", code: "bot_exists" }],
      [json(409, { error: "bot_limit_reached", message: "" }), { kind: "conflict", code: "bot_limit_reached" }],
      [json(409, { error: "bot_disabled", message: "" }), { kind: "conflict", code: "bot_disabled" }],
      [json(409, { error: "something_else", message: "" }), { kind: "failed", status: 409 }],
      [json(503, { error: "bots_unavailable", message: "" }), { kind: "failed", status: 503 }],
      [new Response("bad gateway", { status: 502 }), { kind: "failed", status: 502 }],
    ];
    for (const [response, expected] of cases) {
      const { fetchImpl } = recordingFetch(() => response);
      expect(await createBot(ctx(fetchImpl), { name: "standup" })).toEqual(expected as never);
    }

    const offline = mock(async () => {
      throw new TypeError("network down");
    }) as unknown as typeof fetch;
    expect(await listBots(ctx(offline))).toEqual({ kind: "failed", status: null });
  });

  test("maps 409 bot_token_limit_reached", async () => {
    const { fetchImpl } = recordingFetch(() => json(409, {
      error: "bot_token_limit_reached",
      message: "the bot already has the maximum number of active tokens",
    }));

    expect(await mintBotToken(ctx(fetchImpl), BOT_JID, { label: "laptop" })).toEqual({
      kind: "conflict",
      code: "bot_token_limit_reached",
    });
  });

  test("malformed body yields failed", async () => {
    const bodies: unknown[] = [
      { bots: "nope", limit: 10, token_limit: 10 },
      { bots: [], limit: 10 },
      { bots: [{ ...botBody, tokens: [{ ...tokenBody, id: 7 }] }], limit: 10, token_limit: 10 },
      { bots: [{ ...botBody, disabled_reason: "banned" }], limit: 10, token_limit: 10 },
    ];
    for (const body of bodies) {
      const { fetchImpl } = recordingFetch(() => json(200, body));
      expect(await listBots(ctx(fetchImpl))).toEqual({ kind: "failed", status: 200 });
    }

    const notJson = recordingFetch(() => new Response("<html>", { status: 200 }));
    expect(await listBots(ctx(notJson.fetchImpl))).toEqual({ kind: "failed", status: 200 });

    const noSecret = recordingFetch(() => json(201, { token: tokenBody }));
    expect(await mintBotToken(ctx(noSecret.fetchImpl), BOT_JID, { label: "laptop" })).toEqual({
      kind: "failed",
      status: 201,
    });
  });

  test("does not report secret in telemetry", async () => {
    const stub = createFaroStub();
    __setFaroForTesting(stub as never);

    const malformed = recordingFetch(() => json(201, { token: { id: 1 }, secret: SECRET }));
    await mintBotToken(ctx(malformed.fetchImpl), BOT_JID, { label: "laptop" });
    const unavailable = recordingFetch(() => json(503, { error: "bots_unavailable", message: "" }));
    await revokeBotToken(ctx(unavailable.fetchImpl), BOT_JID, tokenBody.id);
    const offline = mock(async () => {
      throw new TypeError(`failed ${SECRET}`);
    }) as unknown as typeof fetch;
    await deleteBot(ctx(offline), BOT_JID);
    // Expected client outcomes are not failures and are not reported.
    const conflict = recordingFetch(() => json(409, { error: "bot_exists", message: "" }));
    await createBot(ctx(conflict.fetchImpl), { name: "standup" });

    expect(stub.errors.map((entry) => entry.options?.context)).toEqual([
      { kind: "http.fetch", operation: "bot-token-mint-post", status: "201", recoverable: "false" },
      { kind: "http.fetch", operation: "bot-token-revoke-delete", status: "503", recoverable: "true" },
      { kind: "http.fetch", operation: "bots-delete", status: "0", recoverable: "true" },
    ]);
    const serialized = JSON.stringify(stub.errors.map((entry) => ({
      message: entry.error.message,
      stack: entry.error.stack,
      options: entry.options,
    })));
    expect(serialized).not.toContain(SECRET);
    expect(serialized).not.toContain("wbt_");
    expect(serialized).not.toContain("standup");
    expect(serialized).not.toContain("session-1");
  });

  test("BOT_NAME_PATTERN accepts and rejects the server table", () => {
    for (const rejected of ["", "-a", "a-", "_a", "a_", "A", "a+b", "a.b", "a b", "ä", "a".repeat(33)]) {
      expect(BOT_NAME_PATTERN.test(rejected)).toBe(false);
    }
    for (const accepted of ["standup", "a", "7", "a_b-c", "ci-2", "a".repeat(32)]) {
      expect(BOT_NAME_PATTERN.test(accepted)).toBe(true);
    }
  });
});

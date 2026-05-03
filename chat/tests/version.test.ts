import { describe, expect, test } from "bun:test";
import { effectScope, ref, type Ref } from "vue";
import {
  extractServerSha,
  extractServerShortVersion,
  useVersion,
  type ServerVersion,
} from "../src/composables/useVersion";
import type { BrowserXmppClient } from "../src/lib/xmpp-client";

describe("server version helpers", () => {
  test("extractServerSha accepts raw full and short SHAs", () => {
    expect(extractServerSha({ version: "deadbeef1234567890abcdef" })).toBe(
      "deadbeef1234567890abcdef",
    );
    expect(extractServerSha({ version: "abc123d" })).toBe("abc123d");
  });

  test("extractServerSha accepts legacy parenthesized SHAs", () => {
    expect(extractServerSha({ version: "0.1.0 (deadbeef1234567890abcdef)" })).toBe(
      "deadbeef1234567890abcdef",
    );
  });

  test("extractServerShortVersion preserves package fallback values", () => {
    expect(extractServerShortVersion({ version: "0.1.0" })).toBe("0.1.0");
    expect(extractServerShortVersion({ version: "0.1.0 (deadbeef123456)" })).toBe("0.1.0");
  });

  test("useVersion ignores pending fetches after disposal", async () => {
    let resolveVersion!: (value: ServerVersion) => void;
    const pendingVersion = new Promise<ServerVersion>((resolve) => {
      resolveVersion = resolve;
    });
    const client = {
      getServerVersion: () => pendingVersion,
    };
    const clientRef = ref(client) as Ref<BrowserXmppClient | null>;
    const scope = effectScope();
    const state = scope.run(() => useVersion(clientRef))!;

    scope.stop();
    resolveVersion({ version: "deadbeef1234567890abcdef" });
    await pendingVersion;
    await Promise.resolve();

    expect(state.serverVersion.value).toBeNull();
  });
});

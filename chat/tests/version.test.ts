import { describe, expect, test } from "bun:test";
import { effectScope, ref, type Ref } from "vue";
import {
  extractServerCommitSha,
  extractServerReleaseVersion,
  useDeploymentVersionInfo,
  type XmppServerVersion,
} from "../src/shell/version";
import type { BrowserXmppClient } from "../src/lib/xmpp-client";

describe("server version helpers", () => {
  test("extractServerCommitSha accepts raw full and short SHAs", () => {
    expect(extractServerCommitSha({ version: "deadbeef1234567890abcdef" })).toBe(
      "deadbeef1234567890abcdef",
    );
    expect(extractServerCommitSha({ version: "abc123d" })).toBe("abc123d");
  });

  test("extractServerCommitSha accepts legacy parenthesized SHAs", () => {
    expect(extractServerCommitSha({ version: "0.1.0 (deadbeef1234567890abcdef)" })).toBe(
      "deadbeef1234567890abcdef",
    );
  });

  test("extractServerReleaseVersion preserves package fallback values", () => {
    expect(extractServerReleaseVersion({ version: "0.1.0" })).toBe("0.1.0");
    expect(extractServerReleaseVersion({ version: "0.1.0 (deadbeef123456)" })).toBe("0.1.0");
  });

  test("useDeploymentVersionInfo ignores pending fetches after disposal", async () => {
    let resolveVersion!: (value: XmppServerVersion) => void;
    const pendingVersion = new Promise<XmppServerVersion>((resolve) => {
      resolveVersion = resolve;
    });
    const client = {
      getServerVersion: () => pendingVersion,
    };
    const clientRef = ref(client) as Ref<BrowserXmppClient | null>;
    const scope = effectScope();
    const state = scope.run(() => useDeploymentVersionInfo(clientRef))!;

    scope.stop();
    resolveVersion({ version: "deadbeef1234567890abcdef" });
    await pendingVersion;
    await Promise.resolve();

    expect(state.serverVersion.value).toBeNull();
  });
});

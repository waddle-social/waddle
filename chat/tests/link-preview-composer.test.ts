import { describe, expect, test } from "bun:test";
import { effectScope, nextTick, ref } from "vue";
import {
  composerLinkPreviewUrl,
  linkPreviewPayloadFromLookup,
  linkPreviewStateFromLookup,
  sendPayloadFromState,
} from "../src/lib/link-preview-composer";
import { useComposerLinkPreview } from "../src/lib/use-composer-link-preview";
import type { LinkPreviewLookupResult } from "../src/lib/xmpp/link-preview";

describe("composer link preview state", () => {
  test("tracks only the first eligible HTTPS URL", () => {
    expect(composerLinkPreviewUrl("http://ignored.example then https://first.example/a and https://second.example/b"))
      .toBe("https://first.example/a");
  });

  test("projects ready lookup data into a send token and optimistic preview", () => {
    const payload = linkPreviewPayloadFromLookup({
      status: "ready",
      token: "token-1",
      originalUrl: "https://example.com/a",
      normalizedUrl: "https://example.com/a",
      expiresAt: "2999-01-01T00:00:00.000Z",
      title: "Example",
      description: "Plain text",
    });

    expect(payload).toEqual({
      token: "token-1",
      expiresAt: "2999-01-01T00:00:00.000Z",
      preview: {
        originalUrl: "https://example.com/a",
        normalizedUrl: "https://example.com/a",
        title: "Example",
        description: "Plain text",
      },
    });
  });

  test("unsupported and dismissed states fail open with no send payload", () => {
    const unsupported = linkPreviewStateFromLookup("https://example.com/a", {
      status: "not_found",
      originalUrl: "https://example.com/a",
    });

    expect(unsupported).toEqual({ kind: "unsupported", url: "https://example.com/a" });
    expect(sendPayloadFromState(unsupported)).toBeUndefined();
    expect(sendPayloadFromState({ kind: "dismissed", url: "https://example.com/a" })).toBeUndefined();
  });

  test("expired ready states fail open with no send payload", () => {
    expect(sendPayloadFromState({
      kind: "ready",
      url: "https://example.com/a",
      payload: {
        token: "expired-token",
        expiresAt: "2000-01-01T00:00:00.000Z",
        preview: {
          originalUrl: "https://example.com/a",
          normalizedUrl: "https://example.com/a",
        },
      },
    })).toBeUndefined();
  });

  test("failed lookup state fails open with no send payload", () => {
    const failed = linkPreviewStateFromLookup("https://example.com/a", null);

    expect(failed).toEqual({ kind: "failed", url: "https://example.com/a" });
    expect(sendPayloadFromState(failed)).toBeUndefined();
  });

  test("dismissed and loading composer states emit no send payload", async () => {
    const h = setupComposerPreviewHarness(() => new Promise<never>(() => {}));

    await nextTick();

    expect(h.preview.state.value).toEqual({ kind: "loading", url: "https://example.com/a" });
    expect(h.preview.sendPayload.value).toBeUndefined();

    h.preview.dismiss();

    expect(h.preview.state.value).toEqual({ kind: "dismissed", url: "https://example.com/a" });
    expect(h.preview.sendPayload.value).toBeUndefined();
    h.stop();
  });

  test("unsupported and failed composer lookup states emit no send payload", async () => {
    const unsupported = setupComposerPreviewHarness(async () => ({
      status: "not_found",
      originalUrl: "https://example.com/a",
    }));

    await flushComposerPreview();

    expect(unsupported.preview.state.value).toEqual({ kind: "unsupported", url: "https://example.com/a" });
    expect(unsupported.preview.sendPayload.value).toBeUndefined();
    unsupported.stop();

    const failed = setupComposerPreviewHarness(async () => {
      throw new Error("lookup failed");
    });

    await flushComposerPreview();

    expect(failed.preview.state.value).toEqual({ kind: "failed", url: "https://example.com/a" });
    expect(failed.preview.sendPayload.value).toBeUndefined();
    failed.stop();
  });

  test("scope changes clear stale ready payloads and trigger a fresh lookup", async () => {
    const lookups: string[] = [];
    const resolvers: Array<(result: LinkPreviewLookupResult) => void> = [];
    const h = setupComposerPreviewHarness((_body, scope) => {
      lookups.push(scope);
      return new Promise<LinkPreviewLookupResult>((resolve) => resolvers.push(resolve));
    });

    await nextTick();
    expect(h.preview.state.value).toEqual({ kind: "loading", url: "https://example.com/a" });
    resolvers[0]?.(readyLookup("token-a"));
    await flushComposerPreview();

    expect(h.preview.sendPayload.value?.token).toBe("token-a");

    h.scope.value = "room-b@muc.example.com";
    await nextTick();

    expect(h.preview.state.value).toEqual({ kind: "loading", url: "https://example.com/a" });
    expect(h.preview.sendPayload.value).toBeUndefined();

    resolvers[1]?.(readyLookup("token-b"));
    await flushComposerPreview();

    expect(lookups).toEqual(["room-a@muc.example.com", "room-b@muc.example.com"]);
    expect(h.preview.sendPayload.value?.token).toBe("token-b");
    h.stop();
  });

  test("lookup changes clear stale ready payloads without overriding dismissal", async () => {
    const resolvers: Array<(result: LinkPreviewLookupResult) => void> = [];
    const h = setupComposerPreviewHarness(() => (
      new Promise<LinkPreviewLookupResult>((resolve) => resolvers.push(resolve))
    ));

    await nextTick();
    resolvers[0]?.(readyLookup("token-a"));
    await flushComposerPreview();
    expect(h.preview.sendPayload.value?.token).toBe("token-a");

    h.lookup.value = () => new Promise<LinkPreviewLookupResult>((resolve) => resolvers.push(resolve));
    await nextTick();

    expect(h.preview.state.value).toEqual({ kind: "loading", url: "https://example.com/a" });
    expect(h.preview.sendPayload.value).toBeUndefined();

    resolvers[1]?.(readyLookup("token-b"));
    await flushComposerPreview();
    expect(h.preview.sendPayload.value?.token).toBe("token-b");

    h.preview.dismiss();
    h.lookup.value = async () => readyLookup("token-c");
    await nextTick();

    expect(h.preview.state.value).toEqual({ kind: "dismissed", url: "https://example.com/a" });
    expect(h.preview.sendPayload.value).toBeUndefined();
    h.stop();
  });
});

function setupComposerPreviewHarness(
  lookupImpl: (body: string, scope: string) => Promise<LinkPreviewLookupResult | null>,
) {
  const draft = ref("read https://example.com/a");
  const scope = ref("room-a@muc.example.com");
  const lookup = ref((body: string) => lookupImpl(body, scope.value));
  const scopeHandle = effectScope();
  let preview!: ReturnType<typeof useComposerLinkPreview>;
  scopeHandle.run(() => {
    preview = useComposerLinkPreview(draft, lookup, scope);
  });
  return {
    draft,
    lookup,
    scope,
    preview,
    stop: () => scopeHandle.stop(),
  };
}

function readyLookup(token: string): LinkPreviewLookupResult {
  return {
    status: "ready",
    token,
    originalUrl: "https://example.com/a",
    normalizedUrl: "https://example.com/a",
    expiresAt: "2999-01-01T00:00:00.000Z",
    title: "Example",
  };
}

async function flushComposerPreview() {
  await Promise.resolve();
  await nextTick();
}

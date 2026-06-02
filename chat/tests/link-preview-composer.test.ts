import { describe, expect, test } from "bun:test";
import { effectScope, nextTick, ref } from "vue";
import {
  composerLinkPreviewUrl,
  linkPreviewPayloadFromLookup,
  linkPreviewStateFromLookup,
  sendPayloadFromState,
} from "../src/lib/link-preview-composer";
import { prepareComposerSendEvent } from "../src/lib/composer-send-preparation";
import { SEND_LOOKUP_GRACE_MS, useComposerLinkPreview } from "../src/lib/use-composer-link-preview";
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

  test("projects cached image metadata into the optimistic preview", () => {
    const payload = linkPreviewPayloadFromLookup({
      status: "ready",
      token: "token-1",
      originalUrl: "https://example.com/a",
      normalizedUrl: "https://example.com/a",
      expiresAt: "2999-01-01T00:00:00.000Z",
      title: "Example",
      image: {
        url: "https://waddle.example/api/files/11111111-1111-4111-8111-111111111111/link-preview-86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16.png",
        mediaType: "image/png",
        width: 640,
        height: 360,
        alt: "Article screenshot",
      },
    });

    expect(payload?.preview.image).toEqual({
      url: "https://waddle.example/api/files/11111111-1111-4111-8111-111111111111/link-preview-86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16.png",
      mediaType: "image/png",
      width: 640,
      height: 360,
      alt: "Article screenshot",
    });
  });

  test("normal unsupported resolver states fail open with no send payload", () => {
    for (const status of ["unsupported", "blocked"] as const) {
      const unsupported = linkPreviewStateFromLookup("https://example.com/a", {
        status,
        originalUrl: "https://example.com/a",
      });

      expect(unsupported).toEqual({ kind: "unsupported", url: "https://example.com/a" });
      expect(sendPayloadFromState(unsupported)).toBeUndefined();
    }
  });

  test("dismissed states fail open with no send payload", () => {
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
    expect(h.preview.canDismiss.value).toBe(true);
    expect(h.preview.sendPayload.value).toBeUndefined();

    h.preview.dismiss();

    expect(h.preview.state.value).toEqual({ kind: "dismissed", url: "https://example.com/a" });
    expect(h.preview.canDismiss.value).toBe(false);
    expect(h.preview.sendPayload.value).toBeUndefined();
    h.stop();
  });

  test("unsupported and failed composer lookup states emit no send payload", async () => {
    for (const status of ["unsupported", "blocked"] as const) {
      const unsupported = setupComposerPreviewHarness(async () => ({
        status,
        originalUrl: "https://example.com/a",
      }));

      await flushComposerPreview();

      expect(unsupported.preview.state.value).toEqual({ kind: "unsupported", url: "https://example.com/a" });
      expect(unsupported.preview.sendPayload.value).toBeUndefined();
      unsupported.stop();
    }

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

  test("sendPayloadFor waits for the active lookup before quick sends", async () => {
    const resolvers: Array<(result: LinkPreviewLookupResult) => void> = [];
    const h = setupComposerPreviewHarness(() => (
      new Promise<LinkPreviewLookupResult>((resolve) => resolvers.push(resolve))
    ));

    await nextTick();
    expect(h.preview.state.value).toEqual({ kind: "loading", url: "https://example.com/a" });

    let settled = false;
    const payloadPromise = h.preview
      .sendPayloadFor("read https://example.com/a")
      .then((payload) => {
        settled = true;
        return payload;
      });

    await Promise.resolve();
    expect(settled).toBe(false);

    resolvers[0]?.(readyLookup("token-a"));
    await flushComposerPreview();

    expect(await payloadPromise).toEqual({
      token: "token-a",
      expiresAt: "2999-01-01T00:00:00.000Z",
      preview: {
        originalUrl: "https://example.com/a",
        normalizedUrl: "https://example.com/a",
        title: "Example",
      },
    });
    h.stop();
  });

  test("sendPayloadFor keeps the send-time preview when the draft changes while waiting", async () => {
    const resolvers: Array<(result: LinkPreviewLookupResult) => void> = [];
    const h = setupComposerPreviewHarness(() => (
      new Promise<LinkPreviewLookupResult>((resolve) => resolvers.push(resolve))
    ));

    await nextTick();
    expect(h.preview.state.value).toEqual({ kind: "loading", url: "https://example.com/a" });

    const payloadPromise = h.preview.sendPayloadFor("read https://example.com/a");
    h.draft.value = "read https://other.example/a";
    await nextTick();
    expect(resolvers).toHaveLength(2);
    expect(h.preview.state.value).toEqual({ kind: "loading", url: "https://other.example/a" });

    resolvers[0]?.(readyLookup("token-a"));
    await flushComposerPreview();
    expect(h.preview.state.value).toEqual({ kind: "loading", url: "https://other.example/a" });

    expect(await payloadPromise).toEqual({
      token: "token-a",
      expiresAt: "2999-01-01T00:00:00.000Z",
      preview: {
        originalUrl: "https://example.com/a",
        normalizedUrl: "https://example.com/a",
        title: "Example",
      },
    });

    resolvers[1]?.(readyLookup("token-b", "https://other.example/a"));
    await flushComposerPreview();
    expect(h.preview.state.value).toEqual({
      kind: "ready",
      url: "https://other.example/a",
      payload: {
        token: "token-b",
        expiresAt: "2999-01-01T00:00:00.000Z",
        preview: {
          originalUrl: "https://other.example/a",
          normalizedUrl: "https://other.example/a",
          title: "Example",
        },
      },
    });
    h.stop();
  });

  test("sendPayloadFor ignores in-flight metadata for a different body URL", async () => {
    const resolvers: Array<(result: LinkPreviewLookupResult) => void> = [];
    const h = setupComposerPreviewHarness(() => (
      new Promise<LinkPreviewLookupResult>((resolve) => resolvers.push(resolve))
    ));

    await nextTick();

    expect(await h.preview.sendPayloadFor("read https://other.example/a")).toBeUndefined();

    resolvers[0]?.(readyLookup("token-a"));
    await flushComposerPreview();
    h.stop();
  });

  test("sendPayloadFor fails open when the active lookup never settles", async () => {
    const h = setupComposerPreviewHarness(() => new Promise<never>(() => {}));

    await nextTick();
    const timers = installFakeTimeouts();

    try {
      let settled = false;
      const payloadPromise = h.preview.sendPayloadFor("read https://example.com/a").then((payload) => {
        settled = true;
        return payload;
      });

      await Promise.resolve();
      expect(timers.delays).toEqual([SEND_LOOKUP_GRACE_MS]);
      expect(settled).toBe(false);

      timers.runNext();
      expect(await payloadPromise).toBeUndefined();
      expect(settled).toBe(true);
    } finally {
      timers.restore();
      h.stop();
    }
  });

  test("prepareComposerSendEvent forwards the awaited preview payload with the send body", async () => {
    let resolvePreview: (payload: LinkPreviewLookupResult) => void = () => {};
    const bodies: string[] = [];
    const file = new Blob(["hello"], { type: "text/plain" });
    const serialized = {
      body: "read https://example.com/a",
      markup: [{ type: "span" as const, start: 0, end: 4, styles: ["strong" as const] }],
      references: [{ type: "data", uri: "https://example.com/a", begin: 5, end: 26 }],
    };

    const preparedPromise = prepareComposerSendEvent({
      serialized,
      files: [file],
      linkPreviewForBody: async (body) => {
        bodies.push(body);
        const lookup = await new Promise<LinkPreviewLookupResult>((resolve) => {
          resolvePreview = resolve;
        });
        return linkPreviewPayloadFromLookup(lookup) ?? undefined;
      },
    });

    await Promise.resolve();
    expect(bodies).toEqual(["read https://example.com/a"]);

    resolvePreview(readyLookup("token-a"));

    expect(await preparedPromise).toEqual({
      ...serialized,
      files: [file],
      linkPreview: {
        token: "token-a",
        expiresAt: "2999-01-01T00:00:00.000Z",
        preview: {
          originalUrl: "https://example.com/a",
          normalizedUrl: "https://example.com/a",
          title: "Example",
        },
      },
    });
  });

  test("MessageComposer disables mutable controls while preparing a send", async () => {
    const source = await Bun.file(
      new URL("../src/components/chat/MessageComposer.vue", import.meta.url),
    ).text();

    expect(source).toContain(":disabled=\"disabled || isSendBusy\"");
    expect(source).toContain(":disabled=\"disabled || slowModeCooldown > 0 || isPreparingSend\"");
    expect(source).toContain("prepareComposerSendEvent({");
    expect(source).toContain("linkPreviewForBody: linkPreview.sendPayloadFor");
    expect(source).toContain("prepared.linkPreview");
    expect(source).toContain("if (action === \"cancel-reply\" && !isPreparingSend.value)");
    expect(source).toContain("function addAttachments(files: Array<File | Blob>) {\n  if (isPreparingSend.value) return;");
    expect(source).toContain("function onGifSelected(url: string) {\n  if (isPreparingSend.value) return;");
    expect(source).toContain("watch(isPreparingSend, (preparing) => {\n  if (preparing) showGifPicker.value = false;");
    expect(source.match(/:disabled="disabled \|\| isPreparingSend"/g)?.length ?? 0).toBeGreaterThanOrEqual(4);
    expect(source.match(/:disabled="isPreparingSend"/g)?.length ?? 0).toBeGreaterThanOrEqual(2);
  });

  test("MessageCard inline edits reuse composer preview lookup and emit payloads", async () => {
    const source = await Bun.file(
      new URL("../src/components/chat/MessageCard.vue", import.meta.url),
    ).text();

    expect(source).toContain("useComposerLinkPreview(");
    expect(source).toContain("@update=\"updateEditDraft\"");
    expect(source).toContain("editLinkPreview.sendPayloadFor(body)");
    expect(source).toContain("if (!isEditing.value || editDraft.value !== draftAtSubmit) return;");
    expect(source).toContain("originalPreviewUrl !== null && !body.includes(originalPreviewUrl)");
    expect(source).not.toContain("originalPreviewUrl ?? \"\\0\"");
    expect(source).toContain("emit(\"edit\", props.message.id, body, markup, references, linkPreview)");
    expect(source).toContain("@click=\"editLinkPreview.dismiss\"");
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

function readyLookup(token: string, url = "https://example.com/a"): LinkPreviewLookupResult {
  return {
    status: "ready",
    token,
    originalUrl: url,
    normalizedUrl: url,
    expiresAt: "2999-01-01T00:00:00.000Z",
    title: "Example",
  };
}

async function flushComposerPreview() {
  await Promise.resolve();
  await nextTick();
}

function installFakeTimeouts() {
  const originalSetTimeout = globalThis.setTimeout;
  const originalClearTimeout = globalThis.clearTimeout;
  const delays: number[] = [];
  const pending = new Map<number, () => void>();
  let nextId = 1;

  globalThis.setTimeout = ((handler: (...args: unknown[]) => void, timeout?: number, ...args: unknown[]) => {
    const id = nextId++;
    delays.push(timeout ?? 0);
    pending.set(id, () => handler(...args));
    return id as unknown as ReturnType<typeof setTimeout>;
  }) as typeof setTimeout;

  globalThis.clearTimeout = ((id?: Parameters<typeof clearTimeout>[0]) => {
    pending.delete(id as unknown as number);
  }) as typeof clearTimeout;

  return {
    delays,
    runNext() {
      const id = pending.keys().next().value as number | undefined;
      if (id === undefined) return;
      const callback = pending.get(id);
      pending.delete(id);
      callback?.();
    },
    restore() {
      globalThis.setTimeout = originalSetTimeout;
      globalThis.clearTimeout = originalClearTimeout;
    },
  };
}

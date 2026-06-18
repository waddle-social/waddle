import { afterEach, describe, expect, mock, test } from "bun:test";
import type { CallTileModel } from "../src/lib/calls/call-tiles";
import {
  $callPictureInPictureActive,
  $callPictureInPictureMode,
  detectCallPictureInPictureSupport,
  enterDocumentCallPictureInPicture,
  enterVideoCallPictureInPicture,
  exitVideoCallPictureInPicture,
  installVideoPictureInPictureCloseHandlers,
  selectCallPictureInPictureTile,
} from "../src/lib/calls/call-picture-in-picture";

const originalWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
const originalDocument = Object.getOwnPropertyDescriptor(globalThis, "document");

afterEach(() => {
  restoreDescriptor(globalThis, "window", originalWindow);
  restoreDescriptor(globalThis, "document", originalDocument);
  $callPictureInPictureActive.set(false);
  $callPictureInPictureMode.set(null);
});

describe("call Picture-in-Picture controller", () => {
  test("prefers Document PiP, falls back to video PiP, and hides unsupported controls", () => {
    expect(
      detectCallPictureInPictureSupport({
        hasDocumentPictureInPicture: true,
        hasStandardVideoPictureInPicture: true,
        hasWebKitVideoPictureInPicture: true,
      }),
    ).toBe("document");

    expect(
      detectCallPictureInPictureSupport({
        hasDocumentPictureInPicture: false,
        hasStandardVideoPictureInPicture: true,
        hasWebKitVideoPictureInPicture: false,
      }),
    ).toBe("video");

    expect(
      detectCallPictureInPictureSupport({
        hasDocumentPictureInPicture: false,
        hasStandardVideoPictureInPicture: false,
        hasWebKitVideoPictureInPicture: true,
      }),
    ).toBe("video");

    expect(
      detectCallPictureInPictureSupport({
        hasDocumentPictureInPicture: false,
        hasStandardVideoPictureInPicture: false,
        hasWebKitVideoPictureInPicture: false,
      }),
    ).toBe("none");
  });

  test("selects a pinned video tile before falling back to the active speaker camera", () => {
    const tiles = [
      tile("self:alice@example.com/web:camera", "alice@example.com/web", "camera", false),
      tile("remote:bob@example.com/web:camera", "bob@example.com/web", "camera", true),
      tile("remote:carol@example.com/web:camera", "carol@example.com/web", "camera", true),
    ];

    expect(
      selectCallPictureInPictureTile({
        tiles,
        pinnedTileKey: "remote:bob@example.com/web:camera",
        activeSpeakerIdentities: new Set(["carol@example.com/web"]),
      })?.key,
    ).toBe("remote:bob@example.com/web:camera");

    expect(
      selectCallPictureInPictureTile({
        tiles,
        pinnedTileKey: null,
        activeSpeakerIdentities: new Set(["carol@example.com/web"]),
      })?.key,
    ).toBe("remote:carol@example.com/web:camera");
  });

  test("enters standard or WebKit video Picture-in-Picture through one controller", async () => {
    const standardCalls: string[] = [];
    await enterVideoCallPictureInPicture({
      requestPictureInPicture: async () => {
        standardCalls.push("standard");
      },
    } as HTMLVideoElement);
    expect(standardCalls).toEqual(["standard"]);

    const webKitCalls: string[] = [];
    await enterVideoCallPictureInPicture({
      webkitSupportsPresentationMode: (mode: string) => mode === "picture-in-picture",
      webkitSetPresentationMode: (mode: string) => {
        webKitCalls.push(mode);
      },
    } as unknown as HTMLVideoElement);
    expect(webKitCalls).toEqual(["picture-in-picture"]);
  });

  test("moves the panel into a Document PiP window and records document mode", async () => {
    const panel = {};
    const appended: unknown[] = [];
    const pipDocument = {
      body: {
        className: "",
        appendChild: (node: unknown) => {
          appended.push(node);
          return node;
        },
        contains: (node: unknown) => appended.includes(node),
      },
      head: { appendChild: (node: unknown) => node },
      createElement: (tagName: string) => ({ tagName }),
    };
    const requestWindow = mock(async (options?: { width?: number; height?: number }) => {
      expect(options).toEqual({ width: 360, height: 240 });
      return { document: pipDocument } as Window;
    });
    Object.defineProperty(globalThis, "window", {
      configurable: true,
      value: { documentPictureInPicture: { requestWindow } },
    });
    Object.defineProperty(globalThis, "document", {
      configurable: true,
      value: { styleSheets: [] },
    });

    const pipWindow = await enterDocumentCallPictureInPicture(panel as HTMLElement, {
      width: 360,
      height: 240,
    });

    expect(pipWindow.document).toBe(pipDocument);
    expect(pipDocument.body.contains(panel)).toBe(true);
    expect(pipDocument.body.className).toBe("call-pip-document");
    expect($callPictureInPictureActive.get()).toBe(true);
    expect($callPictureInPictureMode.get()).toBe("document");
  });

  test("exits standard and WebKit video Picture-in-Picture", async () => {
    const standardVideo = {};
    const exitPictureInPicture = mock(async () => undefined);
    Object.defineProperty(globalThis, "document", {
      configurable: true,
      value: {
        pictureInPictureElement: standardVideo,
        exitPictureInPicture,
      },
    });

    await exitVideoCallPictureInPicture(standardVideo as HTMLVideoElement);
    expect(exitPictureInPicture).toHaveBeenCalledTimes(1);

    restoreDescriptor(globalThis, "document", originalDocument);
    const webKitCalls: string[] = [];
    await exitVideoCallPictureInPicture({
      webkitSetPresentationMode: (mode: string) => {
        webKitCalls.push(mode);
      },
    } as unknown as HTMLVideoElement);
    expect(webKitCalls).toEqual(["inline"]);
  });

  test("keeps the WebKit close listener through enter and clears it on exit", () => {
    const listeners = new Map<string, Set<() => void>>();
    const video = {
      webkitPresentationMode: "inline",
      addEventListener: (type: string, listener: () => void) => {
        const existing = listeners.get(type) ?? new Set<() => void>();
        existing.add(listener);
        listeners.set(type, existing);
      },
      removeEventListener: (type: string, listener: () => void) => {
        listeners.get(type)?.delete(listener);
      },
    };
    const onClose = mock(() => undefined);

    installVideoPictureInPictureCloseHandlers(video as unknown as HTMLVideoElement, onClose);

    video.webkitPresentationMode = "picture-in-picture";
    dispatch(listeners, "webkitpresentationmodechanged");
    expect(onClose).not.toHaveBeenCalled();
    expect(listeners.get("webkitpresentationmodechanged")?.size).toBe(1);

    video.webkitPresentationMode = "inline";
    dispatch(listeners, "webkitpresentationmodechanged");
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(listeners.get("webkitpresentationmodechanged")?.size).toBe(0);
  });
});

function tile(
  key: string,
  identity: string,
  source: CallTileModel["source"],
  hasVideo: boolean,
): CallTileModel {
  return {
    key,
    identity,
    label: identity.split("@")[0] ?? identity,
    source,
    screenTrackKey: null,
    isSelf: key.startsWith("self:"),
    mirrorVideo: false,
    showsPresentingGlyph: source === "screen_share",
    micEnabledHint: true,
    videoTrack: hasVideo ? ({} as never) : null,
  };
}

function restoreDescriptor(
  target: object,
  key: PropertyKey,
  descriptor: PropertyDescriptor | undefined,
): void {
  if (descriptor) {
    Object.defineProperty(target, key, descriptor);
    return;
  }
  Reflect.deleteProperty(target, key);
}

function dispatch(listeners: Map<string, Set<() => void>>, type: string): void {
  for (const listener of Array.from(listeners.get(type) ?? [])) listener();
}

import { atom } from "nanostores";
import type { CallTileModel } from "./call-tiles";

export type CallPictureInPictureSupport = "document" | "video" | "none";
export type CallPictureInPictureMode = "document" | "video" | null;

export type CallPictureInPictureSupportFlags = {
  hasDocumentPictureInPicture: boolean;
  hasStandardVideoPictureInPicture: boolean;
  hasWebKitVideoPictureInPicture: boolean;
};

export const $callPictureInPictureSupport = atom<CallPictureInPictureSupport>("none");
export const $callPictureInPictureActive = atom<boolean>(false);
export const $callPictureInPictureMode = atom<CallPictureInPictureMode>(null);

export function detectCallPictureInPictureSupport(
  flags: CallPictureInPictureSupportFlags,
): CallPictureInPictureSupport {
  if (flags.hasDocumentPictureInPicture) return "document";
  if (flags.hasStandardVideoPictureInPicture || flags.hasWebKitVideoPictureInPicture) {
    return "video";
  }
  return "none";
}

function detectBrowserCallPictureInPictureSupport(): CallPictureInPictureSupport {
  if (typeof window === "undefined" || typeof document === "undefined") return "none";
  const browserWindow = window as Window & {
    documentPictureInPicture?: {
      requestWindow?: (options?: { width?: number; height?: number }) => Promise<Window>;
    };
  };
  const videoPrototype = HTMLVideoElement.prototype as WebKitPictureInPictureVideoPrototype;
  return detectCallPictureInPictureSupport({
    hasDocumentPictureInPicture:
      typeof browserWindow.documentPictureInPicture?.requestWindow === "function",
    hasStandardVideoPictureInPicture:
      document.pictureInPictureEnabled === true &&
      typeof HTMLVideoElement.prototype.requestPictureInPicture === "function",
    hasWebKitVideoPictureInPicture:
      typeof videoPrototype.webkitSetPresentationMode === "function" &&
      typeof videoPrototype.webkitSupportsPresentationMode === "function",
  });
}

export function refreshCallPictureInPictureSupport(): void {
  $callPictureInPictureSupport.set(detectBrowserCallPictureInPictureSupport());
}

export type SelectCallPictureInPictureTileInput = {
  tiles: readonly CallTileModel[];
  pinnedTileKey: string | null;
  activeSpeakerIdentities: ReadonlySet<string>;
};

export function selectCallPictureInPictureTile(
  input: SelectCallPictureInPictureTileInput,
): CallTileModel | null {
  const videoTiles = input.tiles.filter((tile) => tile.videoTrack !== null);
  const pinned = input.pinnedTileKey
    ? videoTiles.find((tile) => tile.key === input.pinnedTileKey) ?? null
    : null;
  if (pinned) return pinned;

  const activeSpeaker = videoTiles.find((tile) =>
    tile.source === "camera" && input.activeSpeakerIdentities.has(tile.identity)
  ) ?? null;
  if (activeSpeaker) return activeSpeaker;

  return videoTiles.find((tile) => !tile.isSelf && tile.source === "camera")
    ?? videoTiles.find((tile) => tile.source === "camera")
    ?? videoTiles[0]
    ?? null;
}

export async function enterVideoCallPictureInPicture(video: HTMLVideoElement): Promise<void> {
  if (typeof video.requestPictureInPicture === "function") {
    await video.requestPictureInPicture();
    return;
  }
  const webKitVideo = video as WebKitPictureInPictureVideoPrototype;
  if (
    typeof webKitVideo.webkitSetPresentationMode === "function"
  ) {
    if (
      typeof webKitVideo.webkitSupportsPresentationMode !== "function" ||
      !webKitVideo.webkitSupportsPresentationMode("picture-in-picture")
    ) {
      throw new Error("Video Picture-in-Picture is not available for this video element");
    }
    webKitVideo.webkitSetPresentationMode("picture-in-picture");
    return;
  }
  throw new Error("Video Picture-in-Picture is not available");
}

export async function exitVideoCallPictureInPicture(video: HTMLVideoElement | null): Promise<void> {
  if (
    typeof document !== "undefined" &&
    video !== null &&
    document.pictureInPictureElement === video
  ) {
    await document.exitPictureInPicture();
    return;
  }
  const webKitVideo = video as WebKitPictureInPictureVideoPrototype | null;
  if (
    webKitVideo &&
    typeof webKitVideo.webkitSetPresentationMode === "function"
  ) {
    webKitVideo.webkitSetPresentationMode("inline");
  }
}

export function installVideoPictureInPictureCloseHandlers(
  video: HTMLVideoElement,
  onClose: () => void,
): () => void {
  let active = true;
  const cleanup = () => {
    if (!active) return;
    active = false;
    video.removeEventListener("leavepictureinpicture", onStandardLeave);
    video.removeEventListener("webkitpresentationmodechanged", onWebKitModeChange);
  };
  const close = () => {
    cleanup();
    onClose();
  };
  const onStandardLeave = () => close();
  const onWebKitModeChange = () => {
    const webKitVideo = video as HTMLVideoElement & { webkitPresentationMode?: string };
    if (webKitVideo.webkitPresentationMode !== "picture-in-picture") close();
  };

  video.addEventListener("leavepictureinpicture", onStandardLeave);
  video.addEventListener("webkitpresentationmodechanged", onWebKitModeChange);
  return cleanup;
}

export async function enterDocumentCallPictureInPicture(
  panel: HTMLElement,
  options: { width: number; height: number },
): Promise<Window> {
  const openerWindow = window as Window & {
    documentPictureInPicture?: {
      requestWindow: (options?: { width?: number; height?: number }) => Promise<Window>;
    };
  };
  const pipWindow = await openerWindow.documentPictureInPicture?.requestWindow(options);
  if (!pipWindow) {
    throw new Error("Document Picture-in-Picture is not available");
  }
  copyStyleSheets(document, pipWindow.document);
  pipWindow.document.body.className = "call-pip-document";
  pipWindow.document.body.appendChild(panel);
  $callPictureInPictureActive.set(true);
  $callPictureInPictureMode.set("document");
  return pipWindow;
}

export function findCallPictureInPictureVideo(
  root: Pick<Element, "querySelectorAll"> | Pick<Document, "querySelectorAll"> | null,
  tileKey: string,
): HTMLVideoElement | null {
  if (!root) return null;
  for (const video of root.querySelectorAll<HTMLVideoElement>("video[data-call-tile-key]")) {
    if (video.dataset.callTileKey === tileKey) return video;
  }
  return null;
}

function copyStyleSheets(source: Document, target: Document): void {
  for (const styleSheet of Array.from(source.styleSheets)) {
    try {
      const rules = Array.from(styleSheet.cssRules).map((rule) => rule.cssText).join("");
      const style = target.createElement("style");
      style.textContent = rules;
      target.head.appendChild(style);
    } catch {
      if (!styleSheet.href) continue;
      const link = target.createElement("link");
      link.rel = "stylesheet";
      link.href = styleSheet.href;
      target.head.appendChild(link);
    }
  }
}

type WebKitPictureInPictureVideoPrototype = HTMLVideoElement & {
  webkitSupportsPresentationMode?: (mode: "picture-in-picture") => boolean;
  webkitSetPresentationMode?: (mode: "inline" | "picture-in-picture") => void;
};

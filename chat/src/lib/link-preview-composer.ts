import type { LinkPreview } from "@/lib/chat-ui";
import { firstEligibleHttpsUrl, type LinkPreviewLookupResult } from "@/lib/xmpp/link-preview";

export type ComposerLinkPreviewLookup = (body: string) => Promise<LinkPreviewLookupResult | null>;

export interface ComposerLinkPreviewSendPayload {
  token: string;
  expiresAt: string;
  preview: LinkPreview;
}

export type ComposerLinkPreviewState =
  | { kind: "idle" }
  | { kind: "loading"; url: string }
  | { kind: "ready"; url: string; payload: ComposerLinkPreviewSendPayload }
  | { kind: "failed"; url: string }
  | { kind: "unsupported"; url: string }
  | { kind: "dismissed"; url: string };

export function linkPreviewPayloadFromLookup(
  result: LinkPreviewLookupResult | null,
): ComposerLinkPreviewSendPayload | null {
  if (!result || result.status !== "ready") return null;
  return {
    token: result.token,
    expiresAt: result.expiresAt,
    preview: {
      originalUrl: result.originalUrl,
      normalizedUrl: result.normalizedUrl,
      ...(result.title ? { title: result.title } : {}),
      ...(result.description ? { description: result.description } : {}),
      ...(result.image ? { image: result.image } : {}),
    },
  };
}

export function linkPreviewStateFromLookup(
  url: string,
  result: LinkPreviewLookupResult | null,
): ComposerLinkPreviewState {
  const payload = linkPreviewPayloadFromLookup(result);
  if (payload) return { kind: "ready", url, payload };
  return result?.status === "unsupported" || result?.status === "blocked"
    ? { kind: "unsupported", url }
    : { kind: "failed", url };
}

export function composerLinkPreviewUrl(body: string): string | null {
  return firstEligibleHttpsUrl(body);
}

export function sendPayloadFromState(
  state: ComposerLinkPreviewState,
): ComposerLinkPreviewSendPayload | undefined {
  return state.kind === "ready" && composerLinkPreviewPayloadIsFresh(state.payload)
    ? state.payload
    : undefined;
}

export function composerLinkPreviewPayloadIsFresh(
  payload: ComposerLinkPreviewSendPayload | undefined,
  now = new Date(),
): payload is ComposerLinkPreviewSendPayload {
  if (!payload) return false;
  const expiresAtMs = Date.parse(payload.expiresAt);
  return Number.isFinite(expiresAtMs) && expiresAtMs > now.getTime();
}

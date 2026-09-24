import { MAX_FILE_UPLOAD_BYTES } from "@/lib/xmpp/file-upload";
import { readLimitedBody } from "./read-limited-body";

/**
 * Download the original animated GIF behind a pasted "Copy image" so it
 * can be shared through the normal XEP-0363 upload path.
 *
 * The request is anonymous (no cookies, no referrer). Anything that is
 * not an https `image/gif` within the upload size limit resolves to
 * `null`; failures are expected (hosts without CORS, offline) and the
 * caller falls back to the static clipboard image, so nothing throws
 * and nothing is reported to telemetry.
 */

export interface FetchPastedGifOptions {
  signal?: AbortSignal;
  maxBytes?: number;
  timeoutMs?: number;
  fetchImpl?: typeof fetch;
}

const DEFAULT_TIMEOUT_MS = 10_000;
const GIF_MEDIA_TYPE = "image/gif";
const GIF_SIGNATURE = [0x47, 0x49, 0x46, 0x38]; // "GIF8"
const MAX_BASENAME_LENGTH = 80;

export async function fetchPastedGif(url: string, options: FetchPastedGifOptions = {}): Promise<File | null> {
  const target = parseHttpsUrl(url);
  if (!target) return null;
  const maxBytes = options.maxBytes ?? MAX_FILE_UPLOAD_BYTES;
  const timeout = linkedTimeout(options.signal, options.timeoutMs ?? DEFAULT_TIMEOUT_MS);
  try {
    const response = await (options.fetchImpl ?? fetch)(target.toString(), {
      mode: "cors",
      credentials: "omit",
      referrerPolicy: "no-referrer",
      signal: timeout.signal,
    });
    const bytes = await readGifBody(response, maxBytes);
    return bytes ? new File([bytes], pastedGifFileName(target), { type: GIF_MEDIA_TYPE }) : null;
  } catch {
    return null;
  } finally {
    timeout.dispose();
  }
}

async function readGifBody(response: Response, maxBytes: number): Promise<Uint8Array<ArrayBuffer> | null> {
  if (!response.ok || mediaType(response.headers.get("content-type")) !== GIF_MEDIA_TYPE) {
    await response.body?.cancel().catch(() => undefined);
    return null;
  }
  const declared = Number(response.headers.get("content-length") ?? Number.NaN);
  if (Number.isFinite(declared) && declared > maxBytes) {
    await response.body?.cancel().catch(() => undefined);
    return null;
  }
  const bytes = await readLimitedBody(response, maxBytes);
  return bytes && hasGifSignature(bytes) ? bytes : null;
}

function pastedGifFileName(url: URL): string {
  const segment = url.pathname.split("/").filter(Boolean).pop() ?? "";
  const stem = safeDecode(segment).replace(/\.[^.]*$/, "");
  const sanitized = stem
    .replace(/[^A-Za-z0-9_-]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, MAX_BASENAME_LENGTH);
  return `${sanitized || "pasted"}.gif`;
}

function parseHttpsUrl(url: string): URL | null {
  try {
    const parsed = new URL(url);
    return parsed.protocol === "https:" ? parsed : null;
  } catch {
    return null;
  }
}

function mediaType(contentType: string | null): string {
  return (contentType ?? "").split(";")[0].trim().toLowerCase();
}

function hasGifSignature(bytes: Uint8Array): boolean {
  return GIF_SIGNATURE.every((byte, index) => bytes[index] === byte);
}

function safeDecode(segment: string): string {
  try {
    return decodeURIComponent(segment);
  } catch {
    return segment;
  }
}

function linkedTimeout(signal: AbortSignal | undefined, timeoutMs: number): { signal: AbortSignal; dispose: () => void } {
  const controller = new AbortController();
  const abort = () => controller.abort();
  const timer = setTimeout(abort, timeoutMs);
  if (signal?.aborted) abort();
  signal?.addEventListener("abort", abort, { once: true });
  return {
    signal: controller.signal,
    dispose: () => {
      clearTimeout(timer);
      signal?.removeEventListener("abort", abort);
    },
  };
}

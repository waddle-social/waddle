import { defineMiddleware } from "astro:middleware";

// Baseline Content-Security-Policy for every server-rendered HTML document.
//
// Why a middleware header and not Astro's built-in `security.csp`: the app
// uses `<ClientRouter />` (view transitions) and Shiki client-side syntax
// highlighting, both explicitly unsupported by Astro's CSP implementation
// (its auto-generated style hashes would make browsers ignore the
// `'unsafe-inline'` that Shiki's inline styles and Vue SSR style bindings
// require).
//
// Inline `<script>` bodies (the theme-restore `is:inline` script and Astro's
// island/view-transition bootstrap) are hashed per response, so `script-src`
// never needs `'unsafe-inline'`.
const INLINE_SCRIPT_PATTERN = /<script\b(?![^>]*\bsrc\s*=)[^>]*>([\s\S]*?)<\/script>/gi;

function contentSecurityPolicy(inlineScriptHashes: readonly string[]): string {
  const scriptSources = ["'self'", "'wasm-unsafe-eval'", ...inlineScriptHashes.map((hash) => `'${hash}'`)];
  return [
    "default-src 'self'",
    // 'wasm-unsafe-eval' for the XMPP client WASM module; hashes cover the
    // inline theme-restore and Astro bootstrap scripts.
    `script-src ${scriptSources.join(" ")}`,
    // Shiki highlights code blocks with style attributes and Vue binds
    // :style at SSR time; CSP cannot hash style attributes, so inline
    // styles stay allowed. Scripts remain locked down above.
    "style-src 'self' 'unsafe-inline'",
    // Attachments/avatars/GIFs come from arbitrary https upload hosts,
    // decrypted attachments from blob: object URLs, virtual-background
    // custom images from data: URLs.
    "img-src 'self' https: data: blob:",
    // Inline audio/video attachments (https) and decrypted blob: media.
    "media-src 'self' https: blob:",
    "font-src 'self' data:",
    // XMPP WebSocket + HTTP (SERVER_BASE_URL), LiveKit SFU WebSocket, Faro
    // collector, HLS/link-preview/attachment fetches. Hosts are
    // deployment-specific (worker env / server-provided), so scheme-level
    // https:/wss: is the tightest static policy that cannot break calls.
    "connect-src 'self' https: wss: blob:",
    // Service worker ('self') plus blob: workers minted by hls.js, LiveKit
    // track processors, and the noise-suppression AudioWorklet pipeline.
    "worker-src 'self' blob:",
    // PDF attachment iframes (https or decrypted blob:) and allowlisted
    // player embeds (see player-embed-allowlist.ts).
    "frame-src 'self' https: blob:",
    "object-src 'none'",
    "base-uri 'self'",
    "form-action 'self'",
    "frame-ancestors 'none'",
    "manifest-src 'self'",
  ].join("; ");
}

async function sha256Source(text: string): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
  const bytes = new Uint8Array(digest);
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return `sha256-${btoa(binary)}`;
}

async function inlineScriptHashes(html: string): Promise<string[]> {
  const hashes = new Set<string>();
  for (const match of html.matchAll(INLINE_SCRIPT_PATTERN)) {
    const body = match[1];
    if (body) hashes.add(await sha256Source(body));
  }
  return [...hashes];
}

export const onRequest = defineMiddleware(async (_context, next) => {
  const response = await next();
  // The Vite dev server relies on its own inline/eval tooling; enforce the
  // policy only on production builds (`build` + deploy, `preview`).
  if (import.meta.env.DEV) return response;
  const contentType = response.headers.get("content-type") ?? "";
  if (!contentType.toLowerCase().includes("text/html")) return response;

  const html = await response.text();
  const headers = new Headers(response.headers);
  headers.set("Content-Security-Policy", contentSecurityPolicy(await inlineScriptHashes(html)));
  return new Response(html, {
    status: response.status,
    statusText: response.statusText,
    headers,
  });
});

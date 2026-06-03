# Embedded player link previews (click-to-load iframe)

Date: 2026-06-03
Status: Approved design — ready for implementation plan

## Problem

Pasting a YouTube watch link (e.g.
`https://www.youtube.com/watch?v=429A_VugWW0&list=RD429A_VugWW0`) currently
yields a static info card: a proxied `og:image` thumbnail plus `og:title` /
`og:description`. Clicking the card navigates to YouTube. There is no inline
player.

We want the preview to offer an **embedded player** — a click-to-load YouTube
(and, generically, any allowlisted provider) `<iframe>` rendered inline in the
message.

## Goals

- Inline, playable embedded player for YouTube and other providers that expose
  an embeddable player via OpenGraph `og:video` metadata.
- Preserve the existing privacy posture as far as practical: nothing reaches the
  third-party origin until the user explicitly clicks play.
- Stay XEP-conformant: deliver the embed using standard OpenGraph `og:video`
  properties over the existing XEP-0511 pipeline — no new custom namespace.
- Honor the typed-payloads hard rule end to end (no `String`/blob smuggling).

## Non-goals

- oEmbed endpoint discovery/fetch. We rely solely on `og:video` head metadata.
  (YouTube and Vimeo both emit it; revisit only if a desired provider does not.)
- Playlist/radio context. The `list=…` parameter is dropped; we embed the single
  video.
- Re-streaming or proxying the video through Waddle (heavy; violates provider
  ToS).
- Server-advertised allowlist (via disco). The client mirrors the allowlist as a
  small hard-coded constant.

## Key decisions (from brainstorming)

1. **Privacy posture: direct iframe, click-to-load.** Use the provider's embed
   (for YouTube, rewritten to `www.youtube-nocookie.com`). The iframe is only
   created after the user clicks play; the facade shows the Waddle-proxied
   thumbnail. Nothing contacts the provider until opt-in.
2. **Provider scope: any `og:video` iframe, gated by an allowlisted frame
   origin.** The resolver parses `og:video` generically; only embeds whose origin
   matches the allowlist are sealed/rendered.
3. **Allowlist enforcement: server-side, enforced both ends.** The server refuses
   to seal a player token unless the embed origin is allowlisted; the client
   re-checks the origin against a hard-coded mirror before rendering.
4. **Facade UX: full info card + play overlay.** Keep the existing card
   (proxied thumbnail, title, description, site name) and add a play overlay on
   the thumbnail. Clicking swaps the thumbnail region for the iframe; title and
   description remain.

## Architecture & data flow

Unchanged pipeline:
`lookup IQ → resolve → HMAC token → send-time → XEP-0511 stamp → client render`.

The player embed is **additive** to the existing info card — not mutually
exclusive like the direct-video path. A resolved metadata record may carry image
+ title + description + player embed together.

```
client paste URL
  → IQ get urn:waddle:link-preview:0 <lookup>
  → resolver fetches head, scans og:* (incl. og:video:*)
  → if embeddable + origin allowlisted: seal player embed in token
  → client sends message with sealed token
  → server stamps XEP-0511 <rdf:Description> incl. og:video:* children
  → client renders info card + play overlay; click → iframe
```

## Component design

### 1. Server — resolver (`link_preview_resolver.rs`)

- While scanning the head (reusing `meta_content()`), extract:
  - `og:video:secure_url` (fallback `og:video:url`) — the embed URL.
  - `og:video:type` — must be `text/html` to be an iframe player.
  - `og:video:width`, `og:video:height` — for aspect ratio.
- Validate the embed URL through the **same** remote-URL policy as every other
  fetched URL (https only, public IP, SSRF guards).
- Require the embed origin to match the **allowlist**, else drop the embed (the
  preview still ships as a plain info card).

### 2. Server — allowlist (typed config)

A typed server-side list of origin rules:

```rust
struct PlayerEmbedRule {
    /// Origin that must match the og:video embed URL (scheme + host[:port]).
    match_origin: Origin,
    /// Optional canonical host rewrite applied before sealing.
    host_rewrite: Option<Host>,
}
```

Initial rules:

- `https://www.youtube.com` → rewrite host to `www.youtube-nocookie.com`
- `https://www.youtube-nocookie.com` → as-is
- `https://player.vimeo.com` → as-is

The allowlist is the security boundary. Keep it small and explicit. Whether it
is env-overridable (like `WADDLE_LINK_PREVIEW_MAX_HTML_HEAD_BYTES`) is an
implementation detail; default to a compiled-in constant unless ops needs
otherwise.

### 3. Server — typed model & token

- New `ResolvedPlayerEmbed { url: Url, width: Option<u32>, height: Option<u32> }`
  added to `ResolvedLinkMetadata` (alongside `image`, not replacing it).
- New sealed field on `LinkPreviewTokenData` carrying the same typed embed.
- Mutual exclusivity: a *direct video* and a *player embed* are mutually
  exclusive (a link is either a direct media file or an HTML page with a player).
  A player embed coexists with image/title/description.

### 4. Protocol — XEP-0511 (`xep0511.rs`)

Extend `LinkMetadata` with the embed and serialize standard OpenGraph `og:video`
children inside `<rdf:Description>`:

- `og:video` / `og:video:secure_url` — embed URL
- `og:video:type` — `text/html`
- `og:video:width` / `og:video:height`

This is XEP-conformant: `og:video` is canonical OpenGraph and XEP-0511 mirrors
og: properties. No `urn:waddle:*` extension.

### 5. Client — render (`MessageBody.vue`, `chat-ui.ts`)

- Add `playerEmbed?: { url: string; width?: number; height?: number }` to
  `LinkPreview`.
- Add a `"player"` kind to `linkPreviewMediaState()`.
- Render the full info card with a **play overlay** on the proxied thumbnail.
- On click, swap the thumbnail region for an `<iframe>` (reuse the existing
  `playingVideos` gating set so nothing loads until click), keeping title and
  description.
- **Client re-check**: validate the embed origin against a hard-coded mirror of
  the server allowlist before rendering; otherwise fall back to the plain card.
- iframe attributes:
  - `loading="lazy"`
  - `allow="encrypted-media; picture-in-picture; fullscreen"`
  - `allowfullscreen`
  - `referrerpolicy="strict-origin-when-cross-origin"`
  - responsive 16:9 container, sized from `og:video:width/height` when present.

## Security

- Allowlist enforced both ends; embed URL re-validated server-side before sealing.
- Nothing contacts the provider until the user clicks play (facade uses the
  Waddle-proxied thumbnail).
- The chat app has **no app-level CSP today**. Document that if a CSP is later
  added it MUST include `frame-src` for every allowlisted embed origin
  (`https://www.youtube-nocookie.com`, etc.), so the feature does not silently
  break.
- The `list=…` playlist param is dropped to avoid autoplaying a radio mix.

## Testing (hard rules)

### Rust (`server/`)

- Resolver: a YouTube-like page with `og:video:secure_url` →
  sealed player embed with the host rewritten to `youtube-nocookie.com`.
- Allowlist rejection: a page whose `og:video` origin is not allowlisted → embed
  dropped, info card still produced.
- XEP-0511: serialize/parse round-trip for the `og:video:*` children.

### Client (`chat/`)

- `tests/message-body-link-previews.test.ts`:
  - facade renders with the play overlay;
  - click swaps the thumbnail region to an iframe with the expected `src`/attrs;
  - a non-allowlisted embed origin renders as a plain info card (no iframe).

## Open implementation details (decide during planning)

- Exact env-override surface for the allowlist (constant vs configurable).
- Whether the client allowlist mirror lives next to the existing link-preview
  client code or in a shared constant module.

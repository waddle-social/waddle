# Link preview: native `<video>` for `og:video` media streams

Tracking issue: waddle-social/waddle#864

## Problem

A pasted link to an HTML page that advertises a playable media URL via OpenGraph
`og:video` (e.g. `https://rawkode.academy/watch/...`, which exposes an HLS
`stream.m3u8` with `og:video:type=application/x-mpegURL`) renders today as a plain
image card. The stream never plays. It fits none of the three existing preview
shapes:

1. Image card (proxied `og:image`).
2. Direct-video card — only when the *pasted URL itself* is a direct media file
   (`.mp4/.webm/.mov/.m4v/.ogv`); carried as an XEP-0447 inline file-share.
3. Player-embed card — HTML page advertising `og:video:type=text/html` pointing at
   an **allowlisted** iframe origin (YouTube/Vimeo); carried as XEP-0511
   `og:video:type=text/html`.

## Goal

A fourth shape: an HTML page advertising a playable media URL via `og:video`
renders as a **click-to-play native `<video>`** — for any site that serves
conformant metadata, with **no provider allowlist** (the iframe allowlist exists
only because iframes run third-party JS; a native `<video>` does not).

## Design

Discriminate on `og:video:type`:

- `text/html` → iframe `Player` (unchanged).
- `video/mp4 | video/webm | video/ogg | video/quicktime` (PR1) and
  `application/vnd.apple.mpegurl` + aliases (PR2/HLS) → native `Native{media_type}`.

The media URL travels **inside the XEP-0511 card** (`og:video` /
`og:video:secure_url` + `og:video:type`), so there is no cross-URL file-share
correlation problem (the media URL is typically on a different CDN host than the
page). The raw-`.mp4`-paste path stays on XEP-0447.

### Wire / data model

- Resolver: new `ResolvedNativeVideo` on `ResolvedLinkMetadata.native_video`,
  parallel to existing `video` (raw file) and `player_embed` (iframe); mutually
  exclusive by invariant.
- IQ-lookup token: new `native_video` field on `LinkPreviewTokenData`; lookup
  result reuses the `<video>` element for composer parity.
- Send path: `native_video` is stamped as conformant XEP-0511 `og:video` with the
  real `og:video:type` (https + host-policy re-validated at send).
- XEP-0511 `LinkMetadataVideo` becomes a typed `Player` vs `Native{media_type}`;
  `build`/`parse_og_video` branch on `og:video:type`.
- Recipient: `og:video` native types parse into `LinkPreviewData.video`; the client
  unifies `native_video`/`video` into a single `preview.video` at render time.

### Resolution

- Parse `og:video` / `og:video:url` / `og:video:secure_url` + `og:video:type` on
  HTML pages; require https; gate on `video_enabled` + `classify_url_with_policy`
  (operator `allowed_hosts`/`blocked_hosts` + SSRF global-IP checks) — media URL
  treated like `og:image` (cross-origin allowed).
- **Lightweight server verify** of the media URL, run **in parallel** with the
  `og:image` fetch (worst-case resolve stays ~2× timeout): progressive = check
  `Content-Type`; HLS (PR2) = `#EXTM3U` body sniff is authoritative, with the
  mpegurl content-type alias set secondary.
- Poster = proxied `og:image` → proxied `og:video:thumbnail` → none. Never a raw
  origin URL (click-to-load privacy).

### Client render

- Click-to-play native `<video controls playsinline preload="none">` pointed at the
  remote media URL (never proxied). Poster from the proxied image.
- HLS (PR2): native fast path when
  `video.canPlayType('application/vnd.apple.mpegurl')`; otherwise lazy
  `import('hls.js')`. On a fatal hls.js CORS/network error, fall back to the
  image+title+description card with a "Watch on {host}" link. Video bytes are never
  proxied.

## Formats

Progressive (mp4/webm/ogg/quicktime) + HLS (`application/vnd.apple.mpegurl`,
aliases accepted, `#EXTM3U` sniff authoritative). No DASH.

## Config

Reuse the existing `video_enabled` flag; `allowed_hosts`/`blocked_hosts` still apply
per-CDN.

## Security posture

- Server is the trust anchor: client-authored XEP-0511 is stripped and re-stamped;
  the HMAC-signed token is re-validated for scope/sender/host/https at send.
- Native `<video>`/hls.js executes no third-party JavaScript (why no provider
  allowlist for this path).
- Poster always proxied → no third-party origin contact until the user clicks play.
- SSRF: server media-verify reuses the DNS-pinned, global-IP-checked fetch path;
  bytes are never proxied.
- Residual: hls.js fetches manifest-referenced segment URLs (possibly cross-origin),
  bounded by click-to-load + no-JS-exec + verified manifest origin, not
  segment-origin-allowlisted.

## Deferred / open

Optional operator-configurable native-video origin allowlist (empty = open by
default). Default behavior is open (operator host policy only). Not blocking PR1.

## Delivery

- **PR1** (this work): generalized **progressive** `og:video` native — no new
  dependencies; full resolver/token/XEP-0511/recipient/render pipeline + tests.
- **PR2**: HLS — adds `hls.js`, the `Hls` media type, `#EXTM3U` sniff, and the
  Safari native fast path; lands Rawkode.

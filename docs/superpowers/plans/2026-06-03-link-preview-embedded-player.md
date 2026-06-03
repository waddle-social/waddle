# Embedded Player Link Previews Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Render a pasted YouTube (and any allowlisted `og:video`) link as a click-to-load embedded `<iframe>` player instead of a static info card.

**Architecture:** The resolver extracts `og:video` from the page head; if the embed origin is allowlisted (YouTube rewritten to `youtube-nocookie.com`) it seals a typed player embed into the existing HMAC link-preview token. On send the server stamps it as standard OpenGraph `og:video:*` children inside the XEP-0511 `<rdf:Description>` (conformant — no custom namespace). The client parses `og:video` into a `playerEmbed`, re-checks the origin against a hard-coded allowlist mirror, and renders the existing info card with a play overlay that swaps to an `<iframe>` on click. The player embed flows end-to-end through XEP-0511 metadata (unlike direct video, which uses XEP-0447 file-shares).

**Tech Stack:** Rust (crates `waddle-server`, `waddle-xmpp`, `waddle-xmpp-client`, `waddle-xmpp-client-wasm`), TypeScript/Vue 3 (`chat/`), Bun test runner.

**Spec:** `docs/superpowers/specs/2026-06-03-link-preview-embedded-player-design.md`

---

## File Structure

Server (Rust):
- `crates/waddle-server/src/server/routes/websocket/handlers/iq/link_preview_player_embed.rs` — **Create.** Allowlist + host-rewrite + validation. Single responsibility: decide whether an embed URL is allowlisted and what its sealed form is.
- `crates/waddle-server/src/server/routes/websocket/handlers/iq/link_preview_resolver.rs` — **Modify.** Add `ResolvedPlayerEmbed`, parse `og:video`, gate via allowlist.
- `crates/waddle-server/src/server/routes/websocket/handlers/iq/link_preview_lookup.rs` — **Modify.** Map resolved embed → token; emit `<player>` in lookup `<preview>`.
- `crates/waddle-server/src/server/routes/websocket/handlers/iq/mod.rs` — **Modify.** Register new module.
- `crates/waddle-server/src/server/routes/websocket/handlers/message.rs` — **Modify.** Send-time: re-validate embed origin, map token player → `LinkMetadata.video`.
- `crates/waddle-xmpp/src/xep/xep_waddle_link_preview.rs` — **Modify.** `LinkPreviewTokenPlayer` + wire type + seal/unseal.
- `crates/waddle-xmpp/src/xep/xep0511.rs` — **Modify.** `LinkMetadataVideo`, `og:video:*` build + parse, `with_video`.
- `crates/waddle-xmpp-client/src/messaging/parsing/mod.rs` — **Modify.** Parse `og:video` → `LinkPreviewData.player_embed`.
- `crates/waddle-xmpp-client/src/messaging/...` (the `LinkPreviewData` struct definition) — **Modify.** Add `player_embed` field.
- `crates/waddle-xmpp-client-wasm/src/types.rs` — **Modify.** `WaddleLinkPreviewPlayer` + field.
- `crates/waddle-xmpp-client-wasm/src/conversions.rs` — **Modify.** Map `player_embed` → JS.

Client (TypeScript/Vue):
- `chat/src/lib/xmpp/player-embed-allowlist.ts` — **Create.** Hard-coded allowlist mirror + `isAllowedPlayerEmbedOrigin()`.
- `chat/src/lib/chat-ui.ts` — **Modify.** `LinkPreviewPlayer` type, `playerEmbed` field, `"player"` media-state.
- `chat/src/lib/xmpp/wasm-types.ts` — **Modify.** `WasmLinkPreviewPlayer` + field.
- `chat/src/lib/xmpp/wasm-message-codecs.ts` — **Modify.** Map `player_embed` → `playerEmbed` with allowlist filter.
- `chat/src/lib/xmpp/link-preview.ts` — **Modify.** Parse `<player>` from lookup `<preview>` (composer cosmetic).
- `chat/src/components/chat/MessageBody.vue` — **Modify.** Player card render + facade + iframe.
- Test files alongside each.

---

## Type names (consistent across the whole plan)

- Rust resolver: `ResolvedPlayerEmbed { url: Url, width: Option<u32>, height: Option<u32> }`
- Rust token: `LinkPreviewTokenPlayer { url: Url, width: Option<u32>, height: Option<u32> }` / wire `LinkPreviewTokenPlayerWire { url: String, width: Option<u32>, height: Option<u32> }`
- Rust XEP-0511: `LinkMetadataVideo { url: Url, width: Option<u32>, height: Option<u32> }`
- Rust client parse: `LinkPreviewData.player_embed: Option<LinkPreviewPlayer>` where `LinkPreviewPlayer { url: Url, width: Option<u32>, height: Option<u32> }`
- Rust WASM: `WaddleLinkPreviewPlayer { url: String, width: Option<u32>, height: Option<u32> }`
- TS: `LinkPreviewPlayer { url: string; width?: number; height?: number }`, field `playerEmbed`
- TS WASM: `WasmLinkPreviewPlayer { url: string; width?: number | null; height?: number | null }`, field `player`

---

## Task 1: Player-embed allowlist module (server)

**Files:**
- Create: `crates/waddle-server/src/server/routes/websocket/handlers/iq/link_preview_player_embed.rs`
- Modify: `crates/waddle-server/src/server/routes/websocket/handlers/iq/mod.rs`

- [ ] **Step 1: Register the module**

In `.../handlers/iq/mod.rs`, add alongside the other `mod` declarations (search for `mod link_preview_resolver;`):

```rust
mod link_preview_player_embed;
```

- [ ] **Step 2: Write the failing test (create the file with tests first)**

Create `link_preview_player_embed.rs`:

```rust
//! Allowlist for embeddable `og:video` player iframes.
//!
//! The allowlist is the security boundary for the embedded-player feature: only
//! frame origins on this list are ever sealed into a link-preview token or
//! rendered as an `<iframe>`. YouTube watch pages advertise an
//! `https://www.youtube.com/embed/...` player; we rewrite that to the
//! cookie-reduced `www.youtube-nocookie.com` host before sealing.

use url::Url;

/// One allowlist entry: an embed origin we accept, with an optional canonical
/// host rewrite applied before the embed URL is sealed.
struct PlayerEmbedRule {
    /// `scheme://host[:port]` the page-advertised embed URL must match.
    match_origin: &'static str,
    /// Host substituted before sealing (privacy/canonicalization), when set.
    host_rewrite: Option<&'static str>,
}

const PLAYER_EMBED_RULES: &[PlayerEmbedRule] = &[
    PlayerEmbedRule {
        match_origin: "https://www.youtube.com",
        host_rewrite: Some("www.youtube-nocookie.com"),
    },
    PlayerEmbedRule {
        match_origin: "https://www.youtube-nocookie.com",
        host_rewrite: None,
    },
    PlayerEmbedRule {
        match_origin: "https://player.vimeo.com",
        host_rewrite: None,
    },
];

/// Return the `scheme://host[:port]` origin text for `url`, or `None` when the
/// URL has no host (e.g. opaque origins).
fn origin_text(url: &Url) -> Option<String> {
    let host = url.host_str()?;
    match url.port() {
        Some(port) => Some(format!("{}://{}:{}", url.scheme(), host, port)),
        None => Some(format!("{}://{}", url.scheme(), host)),
    }
}

/// Validate a page-advertised embed URL against the allowlist and return the
/// canonical sealed form (host-rewritten when the rule asks for it). Returns
/// `None` when the origin is not allowlisted.
pub(crate) fn normalize_allowed_player_embed(url: &Url) -> Option<Url> {
    if url.scheme() != "https" {
        return None;
    }
    let origin = origin_text(url)?;
    let rule = PLAYER_EMBED_RULES
        .iter()
        .find(|rule| rule.match_origin == origin)?;
    match rule.host_rewrite {
        Some(host) => {
            let mut rewritten = url.clone();
            rewritten.set_host(Some(host)).ok()?;
            Some(rewritten)
        }
        None => Some(url.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_youtube_watch_embed_to_nocookie_host() {
        let url = Url::parse("https://www.youtube.com/embed/429A_VugWW0").expect("url");
        let sealed = normalize_allowed_player_embed(&url).expect("allowlisted");
        assert_eq!(
            sealed.as_str(),
            "https://www.youtube-nocookie.com/embed/429A_VugWW0"
        );
    }

    #[test]
    fn accepts_vimeo_player_without_rewrite() {
        let url = Url::parse("https://player.vimeo.com/video/12345").expect("url");
        let sealed = normalize_allowed_player_embed(&url).expect("allowlisted");
        assert_eq!(sealed, url);
    }

    #[test]
    fn rejects_non_allowlisted_origin() {
        let url = Url::parse("https://evil.example.com/embed/x").expect("url");
        assert!(normalize_allowed_player_embed(&url).is_none());
    }

    #[test]
    fn rejects_non_https_embed() {
        let url = Url::parse("http://www.youtube.com/embed/x").expect("url");
        assert!(normalize_allowed_player_embed(&url).is_none());
    }
}
```

(`is_sealed_player_embed_allowed`, the send-time re-check, is added in Task 5 — the task that first consumes it — so no intermediate commit carries a dead `pub(crate)` function and trips the `-D warnings` clippy gate.)

- [ ] **Step 3: Run tests to verify they pass**

Run: `cd server && cargo test -p waddle-server link_preview_player_embed`
Expected: 4 tests PASS.

- [ ] **Step 4: Do NOT commit standalone**

`normalize_allowed_player_embed` has no consumer yet, so a standalone commit would fail `cargo clippy -p waddle-server -- -D warnings` on `dead_code`. Leave the file staged-but-uncommitted; it is committed together with its first consumer in **Task 3, Step 10** (which adds this file to its `git add`). Run `cargo test` to confirm the unit tests pass, then proceed to Task 2.

---

## Task 2: Token carries the player embed (server)

**Files:**
- Modify: `crates/waddle-xmpp/src/xep/xep_waddle_link_preview.rs`

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)]` module in `xep_waddle_link_preview.rs` (mirror existing token round-trip tests):

```rust
#[test]
fn round_trips_player_embed_in_token() {
    let secret = b"test-secret";
    let data = LinkPreviewTokenData {
        sender_jid: "alice@example.com".parse().expect("jid"),
        scope_jid: "room@muc.example.com".parse().expect("jid"),
        original_url: Url::parse("https://www.youtube.com/watch?v=429A_VugWW0").expect("url"),
        normalized_url: Url::parse("https://www.youtube.com/watch?v=429A_VugWW0").expect("url"),
        title: Some("A video".to_string()),
        description: None,
        image: None,
        video: None,
        player: Some(LinkPreviewTokenPlayer {
            url: Url::parse("https://www.youtube-nocookie.com/embed/429A_VugWW0").expect("url"),
            width: Some(1280),
            height: Some(720),
        }),
        expires_at_unix: 4_102_444_800,
    };
    let token = encode_link_preview_token(&data, secret);
    let decoded = decode_link_preview_token(&token, secret, 0).expect("decode");
    assert_eq!(decoded.player, data.player);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd server && cargo test -p waddle-xmpp round_trips_player_embed_in_token`
Expected: FAIL to COMPILE — `LinkPreviewTokenData` has no field `player`, `LinkPreviewTokenPlayer` undefined.

- [ ] **Step 3: Add the typed struct**

After `LinkPreviewTokenVideo` (ends line 78), add:

```rust
/// Trusted embeddable player iframe sealed inside the token. The URL is an
/// allowlisted, host-rewritten embed origin the client renders in an `<iframe>`
/// on user action. Mutually exclusive with [`LinkPreviewTokenVideo`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkPreviewTokenPlayer {
    pub url: Url,
    pub width: Option<u32>,
    pub height: Option<u32>,
}
```

- [ ] **Step 4: Add the field to `LinkPreviewTokenData`**

In `LinkPreviewTokenData` (lines 46–60), add after the `video` field (before `expires_at_unix`):

```rust
    /// Embeddable player iframe sealed inside the token, when the page
    /// advertises an allowlisted `og:video` player. Mutually exclusive with
    /// `video`.
    pub player: Option<LinkPreviewTokenPlayer>,
```

- [ ] **Step 5: Add the wire type**

After `LinkPreviewTokenVideoWire` (ends line ~94), add:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LinkPreviewTokenPlayerWire {
    url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    height: Option<u32>,
}
```

Add the field to `LinkPreviewTokenWire` (after its `video` field):

```rust
    #[serde(default, skip_serializing_if = "Option::is_none")]
    player: Option<LinkPreviewTokenPlayerWire>,
```

- [ ] **Step 6: Seal the player in `encode_link_preview_token`**

In `encode_link_preview_token`, in the `LinkPreviewTokenWire { ... }` literal, after the `video: ...` mapping add:

```rust
        player: data.player.as_ref().map(|player| LinkPreviewTokenPlayerWire {
            url: player.url.as_str().to_string(),
            width: player.width,
            height: player.height,
        }),
```

- [ ] **Step 7: Unseal the player in `decode_link_preview_token`**

In the returned `LinkPreviewTokenData { ... }`, after the `video: ...` mapping add:

```rust
        player: wire
            .player
            .map(|player| {
                Ok(LinkPreviewTokenPlayer {
                    url: Url::parse(&player.url)
                        .map_err(|_| WaddleLinkPreviewError::InvalidTokenUrl)?,
                    width: player.width,
                    height: player.height,
                })
            })
            .transpose()?,
```

- [ ] **Step 8: Run test to verify it passes + fix other token constructors**

Run: `cd server && cargo test -p waddle-xmpp round_trips_player_embed_in_token`
Expected: PASS. If other call sites construct `LinkPreviewTokenData` literally they now fail to compile — they are addressed in Tasks 3 and 6. Run `cargo build -p waddle-xmpp` to confirm this crate compiles.

- [ ] **Step 9: Run clippy + commit**

```bash
cd /Users/oyr/projects/waddle
cargo clippy -p waddle-xmpp -- -D warnings
git add server/crates/waddle-xmpp/src/xep/xep_waddle_link_preview.rs
git commit -m "feat(server): seal player embed in link-preview token"
```

---

## Task 3: Resolver extracts `og:video` and seals an embed (server)

**Files:**
- Modify: `crates/waddle-server/src/server/routes/websocket/handlers/iq/link_preview_resolver.rs`
- Modify: `crates/waddle-server/src/server/routes/websocket/handlers/iq/link_preview_lookup.rs`

- [ ] **Step 1: Write the failing resolver test**

Add to the `#[cfg(test)]` module in `link_preview_resolver.rs` (mirror `fetches_safe_preview_image_into_content_addressed_waddle_storage` style):

```rust
#[tokio::test(flavor = "current_thread")]
async fn extracts_allowlisted_player_embed_from_og_video() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/watch"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"<html><head>
                  <meta property="og:title" content="A video">
                  <meta property="og:video:secure_url" content="https://www.youtube.com/embed/429A_VugWW0">
                  <meta property="og:video:type" content="text/html">
                  <meta property="og:video:width" content="1280">
                  <meta property="og:video:height" content="720">
                </head></html>"#,
            "text/html; charset=utf-8",
        ))
        .mount(&server)
        .await;
    let policy = LinkPreviewResolverPolicy {
        allow_http_loopback_for_tests: true,
        ..Default::default()
    };
    let url = Url::parse(&format!("{}/watch", server.uri())).expect("url");

    let outcome = resolve_link_preview(&url, &policy).await;

    let LinkPreviewResolverOutcome::Ready(metadata) = outcome else {
        panic!("expected ready outcome, got {outcome:?}");
    };
    let player = metadata.player_embed.expect("player embed");
    assert_eq!(
        player.url.as_str(),
        "https://www.youtube-nocookie.com/embed/429A_VugWW0"
    );
    assert_eq!(player.width, Some(1280));
    assert_eq!(player.height, Some(720));
}

#[tokio::test(flavor = "current_thread")]
async fn drops_non_allowlisted_player_embed_but_keeps_card() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/watch"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"<html><head>
                  <meta property="og:title" content="A video">
                  <meta property="og:video:secure_url" content="https://evil.example.com/embed/x">
                  <meta property="og:video:type" content="text/html">
                </head></html>"#,
            "text/html; charset=utf-8",
        ))
        .mount(&server)
        .await;
    let policy = LinkPreviewResolverPolicy {
        allow_http_loopback_for_tests: true,
        ..Default::default()
    };
    let url = Url::parse(&format!("{}/watch", server.uri())).expect("url");

    let outcome = resolve_link_preview(&url, &policy).await;

    let LinkPreviewResolverOutcome::Ready(metadata) = outcome else {
        panic!("expected ready outcome, got {outcome:?}");
    };
    assert!(metadata.player_embed.is_none());
    assert_eq!(metadata.title.as_deref(), Some("A video"));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd server && cargo test -p waddle-server extracts_allowlisted_player_embed_from_og_video`
Expected: FAIL to COMPILE — `ResolvedLinkMetadata` has no `player_embed`.

- [ ] **Step 3: Add `ResolvedPlayerEmbed` and the field**

After `ResolvedDirectVideo` (ends line 56) add:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResolvedPlayerEmbed {
    /// Allowlisted, host-rewritten embed URL the client renders in an iframe.
    pub url: Url,
    pub width: Option<u32>,
    pub height: Option<u32>,
}
```

In `ResolvedLinkMetadata` (lines 37–47), add after the `video` field:

```rust
    /// Allowlisted embeddable player iframe discovered from `og:video`. Coexists
    /// with image/title/description; mutually exclusive with `video`.
    pub player_embed: Option<ResolvedPlayerEmbed>,
```

- [ ] **Step 4: Import the allowlist helper**

Near the top imports of `link_preview_resolver.rs`, add:

```rust
use super::link_preview_player_embed::normalize_allowed_player_embed;
```

- [ ] **Step 5: Extract `og:video` in `extract_metadata_parts_from_html`**

In `extract_metadata_parts_from_html` (lines 983–1029), after the `image` binding and before the final `Some((ResolvedLinkMetadata { ... }, image))`, add:

```rust
    let player_embed = meta_content(html, "og:video:secure_url", usize::MAX)
        .or_else(|| meta_content(html, "og:video:url", usize::MAX))
        .filter(|_| {
            meta_content(html, "og:video:type", 64)
                .is_some_and(|ty| ty.eq_ignore_ascii_case("text/html"))
        })
        .and_then(|raw| Url::parse(&raw).ok())
        .and_then(|url| normalize_allowed_player_embed(&url))
        .map(|url| ResolvedPlayerEmbed {
            url,
            width: meta_content(html, "og:video:width", 16).and_then(|raw| raw.parse().ok()),
            height: meta_content(html, "og:video:height", 16).and_then(|raw| raw.parse().ok()),
        });
```

Then set `player_embed` in the returned `ResolvedLinkMetadata { ... }` literal (replace `video: None,` line with both fields):

```rust
            image: None,
            video: None,
            player_embed,
```

- [ ] **Step 6: Set `player_embed: None` in the other two `ResolvedLinkMetadata` constructors**

In `resolve_link_preview` there are two literal constructors:
- The `FetchOnceResult::Html` block (around lines 200–226): add `player_embed: None,` — it is overwritten by the metadata merge below; confirm by reading the merge. Actually the HTML branch builds metadata via `extract_metadata_parts_from_html`; the literal there (if any) only needs the field for compilation. Add `player_embed: None,` wherever a `ResolvedLinkMetadata { ... }` literal exists without it.
- The `FetchOnceResult::DirectVideo` block (lines 227–244): add `player_embed: None,` after `video: Some(...)`.

Run `cd server && cargo build -p waddle-server 2>&1 | grep -A3 "missing field"` to find every literal needing the field, and add `player_embed: None,` to each (except the one in Step 5).

- [ ] **Step 7: Carry image into the resolved metadata**

Note `extract_metadata_parts_from_html` returns `image` separately (it is later cached/replaced with a Waddle-proxied URL). The `player_embed` belongs on the metadata struct itself (Step 5), so it survives the image-caching merge. Confirm by reading where the function's tuple is consumed (search `extract_metadata_parts_from_html(`) that the returned `ResolvedLinkMetadata` is the one ultimately returned (mutated to attach the cached image), so `player_embed` persists. No extra change expected; if the merge rebuilds the struct, copy `player_embed` across.

- [ ] **Step 8: Map resolved embed → token in `link_preview_lookup.rs`**

In `link_preview_lookup.rs`, in the `LinkPreviewTokenData { ... }` literal (lines 163–183), after the `video: ...` mapping add:

```rust
        player: metadata.player_embed.map(|player| {
            waddle_xmpp::xep::LinkPreviewTokenPlayer {
                url: player.url,
                width: player.width,
                height: player.height,
            }
        }),
```

Ensure `LinkPreviewTokenPlayer` is exported from `waddle_xmpp::xep` (check `crates/waddle-xmpp/src/xep/mod.rs` re-exports `LinkPreviewTokenVideo`; add `LinkPreviewTokenPlayer` to the same `pub use` list).

- [ ] **Step 9: Run tests to verify they pass**

Run: `cd server && cargo test -p waddle-server extracts_allowlisted_player_embed_from_og_video drops_non_allowlisted_player_embed_but_keeps_card`
Expected: both PASS.

- [ ] **Step 10: Run clippy + commit**

This commit also includes the allowlist module from Task 1 (its first consumer is here, so the committed state is clippy-clean):

```bash
cd /Users/oyr/projects/waddle
cargo clippy -p waddle-server -- -D warnings
git add server/crates/waddle-server/src/server/routes/websocket/handlers/iq/link_preview_player_embed.rs \
        server/crates/waddle-server/src/server/routes/websocket/handlers/iq/mod.rs \
        server/crates/waddle-server/src/server/routes/websocket/handlers/iq/link_preview_resolver.rs \
        server/crates/waddle-server/src/server/routes/websocket/handlers/iq/link_preview_lookup.rs \
        server/crates/waddle-xmpp/src/xep/mod.rs
git commit -m "feat(server): resolve allowlisted og:video player embed into token"
```

---

## Task 4: XEP-0511 carries `og:video` (server build + parse)

**Files:**
- Modify: `crates/waddle-xmpp/src/xep/xep0511.rs`

- [ ] **Step 1: Write the failing round-trip test**

Add to the `#[cfg(test)]` module in `xep0511.rs` (mirror `parses_xep0511_description_with_namespaced_rdf_about`):

```rust
#[test]
fn builds_and_parses_og_video_player_embed() {
    let mut metadata = LinkMetadata::new(
        Url::parse("https://www.youtube.com/watch?v=429A_VugWW0").expect("url"),
    );
    metadata.video = Some(LinkMetadataVideo {
        url: Url::parse("https://www.youtube-nocookie.com/embed/429A_VugWW0").expect("url"),
        width: Some(1280),
        height: Some(720),
    });

    let element = build_link_metadata_element(&metadata);
    let parsed = parse_link_metadata_element(&element).expect("round-trips");

    let video = parsed.video.expect("video embed parsed");
    assert_eq!(
        video.url.as_str(),
        "https://www.youtube-nocookie.com/embed/429A_VugWW0"
    );
    assert_eq!(video.width, Some(1280));
    assert_eq!(video.height, Some(720));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd server && cargo test -p waddle-xmpp builds_and_parses_og_video_player_embed`
Expected: FAIL to COMPILE — `LinkMetadataVideo` undefined, `LinkMetadata` has no `video`.

- [ ] **Step 3: Add namespace constant**

After `NS_OPENGRAPH_IMAGE` (line 23) add:

```rust
/// OpenGraph video structured-property namespace.
pub const NS_OPENGRAPH_VIDEO: &str = "https://ogp.me/ns#video:";
```

- [ ] **Step 4: Add `LinkMetadataVideo` and the field**

After `LinkPreviewImage` (ends line 58) add:

```rust
/// Embeddable `og:video` player (an iframe URL with `og:video:type=text/html`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkMetadataVideo {
    /// Allowlisted embed URL clients render in an iframe on user action.
    pub url: Url,
    pub width: Option<u32>,
    pub height: Option<u32>,
}
```

In `LinkMetadata` (lines 94–108), add after `images`:

```rust
    /// Embeddable player iframe (`og:video`), when present.
    pub video: Option<LinkMetadataVideo>,
```

- [ ] **Step 5: Initialize `video: None` in `LinkMetadata::new`**

Find `LinkMetadata::new` (search `pub fn new(about: Url) -> Self`) and add `video: None,` to its struct literal. Add a builder to match the existing `with_image` style:

```rust
    /// Attach an embeddable player iframe.
    pub fn with_video(mut self, video: LinkMetadataVideo) -> Self {
        self.video = Some(video);
        self
    }
```

- [ ] **Step 6: Serialize `og:video:*` in `build_link_metadata_element`**

In `build_link_metadata_element`, after the `for image in &metadata.images { ... }` loop and before `description` is returned, add:

```rust
    if let Some(video) = &metadata.video {
        append_og_text(&mut description, "video", Some(video.url.as_str()));
        append_og_video_text(&mut description, "secure_url", Some(video.url.as_str()));
        append_og_video_text(&mut description, "type", Some("text/html"));
        append_og_video_number(&mut description, "width", video.width);
        append_og_video_number(&mut description, "height", video.height);
    }
```

Add the `ogv` prefix to the builder where `og`/`ogi` prefixes are declared (in the `Element::builder("Description", ...)` chain):

```rust
        .prefix(Some("ogv".to_string()), NS_OPENGRAPH_VIDEO)
        .expect("static OpenGraph video prefix is unique")
```

Add two helpers next to `append_og_image_text`/`append_og_number` (search for `fn append_og_image_text`):

```rust
fn append_og_video_text(parent: &mut Element, local: &str, value: Option<&str>) {
    if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
        let mut child = Element::builder(local, NS_OPENGRAPH_VIDEO)
            .prefix(Some("ogv".to_string()), NS_OPENGRAPH_VIDEO)
            .expect("static OpenGraph video prefix is unique")
            .build();
        child.append_text_node(value);
        parent.append_child(child);
    }
}

fn append_og_video_number(parent: &mut Element, local: &str, value: Option<u32>) {
    if let Some(value) = value {
        append_og_video_text(parent, local, Some(&value.to_string()));
    }
}
```

(If `append_og_image_text`/`append_og_number` have a different exact body, mirror it — the point is an `NS_OPENGRAPH_VIDEO`-namespaced child with the `ogv` prefix.)

- [ ] **Step 7: Parse `og:video:*` in `parse_link_metadata_element`**

In `parse_link_metadata_element`, after `metadata.images = parse_og_images(elem);` add:

```rust
    metadata.video = parse_og_video(elem);
```

Add the parser next to `parse_og_images`:

```rust
fn parse_og_video(elem: &Element) -> Option<LinkMetadataVideo> {
    let mut url = None;
    let mut secure_url = None;
    let mut is_html = false;
    let mut width = None;
    let mut height = None;
    for child in elem.children() {
        if child.ns() == NS_OPENGRAPH && child.name() == "video" {
            url = Url::parse(child.text().trim()).ok();
            continue;
        }
        if child.ns() != NS_OPENGRAPH_VIDEO {
            continue;
        }
        let value = child.text().trim().to_string();
        match child.name() {
            "secure_url" => secure_url = Url::parse(&value).ok(),
            "type" => is_html = value.eq_ignore_ascii_case("text/html"),
            "width" => width = value.parse().ok(),
            "height" => height = value.parse().ok(),
            _ => {}
        }
    }
    let url = secure_url.or(url)?;
    is_html.then_some(LinkMetadataVideo { url, width, height })
}
```

- [ ] **Step 8: Run test to verify it passes**

Run: `cd server && cargo test -p waddle-xmpp builds_and_parses_og_video_player_embed`
Expected: PASS.

- [ ] **Step 9: Export the new types**

In `crates/waddle-xmpp/src/xep/mod.rs`, add `LinkMetadataVideo` and `NS_OPENGRAPH_VIDEO` to the `pub use` re-exports next to `LinkMetadata`/`LinkPreviewImage`.

- [ ] **Step 10: Run clippy + commit**

```bash
cd /Users/oyr/projects/waddle
cargo clippy -p waddle-xmpp -- -D warnings
git add server/crates/waddle-xmpp/src/xep/xep0511.rs server/crates/waddle-xmpp/src/xep/mod.rs
git commit -m "feat(server): carry og:video player embed in XEP-0511 metadata"
```

---

## Task 5: Send-time stamps the embed from the token (server)

**Files:**
- Modify: `crates/waddle-server/src/server/routes/websocket/handlers/message.rs`

- [ ] **Step 1: Write the failing test**

Find the existing send-path test module in `message.rs` (search `fn ` near `consume_link_preview_request` tests, or `mod tests`). Add a test that builds a message with a token sealing a player and asserts the stamped XEP-0511 contains `og:video`. Mirror the existing direct-video stamping test (search for a test exercising `consume_link_preview_request` with `video`). Concretely:

```rust
#[test]
fn stamps_og_video_for_allowlisted_player_token() {
    // Build a token sealing an allowlisted youtube-nocookie embed for the URL
    // that appears in the message body, then run the send-path consumer and
    // assert the outgoing message carries an XEP-0511 og:video child.
    let secret = b"send-path-secret";
    let sender: BareJid = "alice@example.com".parse().expect("jid");
    let scope: BareJid = "room@muc.example.com".parse().expect("jid");
    let data = LinkPreviewTokenData {
        sender_jid: sender.clone(),
        scope_jid: scope.clone(),
        original_url: Url::parse("https://www.youtube.com/watch?v=429A_VugWW0").expect("url"),
        normalized_url: Url::parse("https://www.youtube.com/watch?v=429A_VugWW0").expect("url"),
        title: Some("A video".to_string()),
        description: None,
        image: None,
        video: None,
        player: Some(waddle_xmpp::xep::LinkPreviewTokenPlayer {
            url: Url::parse("https://www.youtube-nocookie.com/embed/429A_VugWW0").expect("url"),
            width: Some(1280),
            height: Some(720),
        }),
        expires_at_unix: 4_102_444_800,
    };
    let token = encode_link_preview_token(&data, secret);
    // ... build the inbound message with body containing the original_url and the
    //     <preview-request token="..."/> element, mirroring the existing
    //     direct-video send-path test setup in this module, then call the
    //     consume function under test ...
    // Assert: the stamped LinkMetadata element serializes with an og:video child.
    // Use the same assertion helper the direct-video test uses to inspect
    // message.payloads for the built link-metadata element.
}
```

NOTE TO IMPLEMENTER: Read the existing direct-video send-path test in this file first and copy its exact scaffold (message construction, the function name actually called, and how it inspects `message.payloads`). Replace the assertion to check for an `og:video`/`ogv:` child instead of an XEP-0447 file-share. Do not invent a new harness.

- [ ] **Step 2: Run to verify it fails**

Run: `cd server && cargo test -p waddle-server stamps_og_video_for_allowlisted_player_token`
Expected: FAIL — token has `player` field unhandled / no og:video stamped (and any literal `LinkPreviewTokenData` in this module without `player` fails to compile — add `player: None` to those).

- [ ] **Step 3: Add the send-time re-check to the allowlist module**

In `link_preview_player_embed.rs`, add the function (now used here for the first time, so it lands clippy-clean):

```rust
/// Re-validate an already-sealed embed URL (post-rewrite) at send time. True
/// only when the URL's origin is a final allowlisted embed origin.
pub(crate) fn is_sealed_player_embed_allowed(url: &Url) -> bool {
    let Some(origin) = origin_text(url) else {
        return false;
    };
    PLAYER_EMBED_RULES.iter().any(|rule| {
        let final_origin = match rule.host_rewrite {
            Some(host) => format!("https://{host}"),
            None => rule.match_origin.to_string(),
        };
        final_origin == origin
    })
}
```

Add a unit test for it in that module's `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn sealed_check_accepts_final_origins_only() {
        assert!(is_sealed_player_embed_allowed(
            &Url::parse("https://www.youtube-nocookie.com/embed/x").expect("url")
        ));
        assert!(!is_sealed_player_embed_allowed(
            &Url::parse("https://www.youtube.com/embed/x").expect("url")
        ));
        assert!(!is_sealed_player_embed_allowed(
            &Url::parse("https://evil.example.com/embed/x").expect("url")
        ));
    }
```

- [ ] **Step 4: Re-validate + map the embed in `consume_link_preview_request`**

In `message.rs`, import the allowlist re-check:

```rust
use super::iq::link_preview_player_embed::is_sealed_player_embed_allowed;
```

(Adjust the path to reach the `iq` module from `message.rs`; both are under `handlers`. If `link_preview_player_embed` is `pub(crate)`-visible, use the crate path `crate::server::routes::websocket::handlers::iq::link_preview_player_embed::is_sealed_player_embed_allowed`. Make `is_sealed_player_embed_allowed` and the module `pub(crate)` if needed.)

In the `.map(|preview| { ... })` closure (lines 168–211), after the `image` handling block that calls `metadata = metadata.with_image(...)`, add:

```rust
            if let Some(player) = preview.player.filter(|player| {
                is_sealed_player_embed_allowed(&player.url)
            }) {
                metadata = metadata.with_video(waddle_xmpp::xep::LinkMetadataVideo {
                    url: player.url,
                    width: player.width,
                    height: player.height,
                });
            }
```

(`preview` is the decoded `LinkPreviewTokenData`; `metadata` is the `LinkMetadata` being built. The player is additive — it does not affect `video_sharing`.)

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd server && cargo test -p waddle-server stamps_og_video_for_allowlisted_player_token sealed_check_accepts_final_origins_only`
Expected: both PASS.

- [ ] **Step 6: Run clippy + commit**

```bash
cd /Users/oyr/projects/waddle
cargo clippy -p waddle-server -- -D warnings
git add server/crates/waddle-server/src/server/routes/websocket/handlers/iq/link_preview_player_embed.rs \
        server/crates/waddle-server/src/server/routes/websocket/handlers/message.rs
git commit -m "feat(server): stamp og:video player embed on the send path"
```

---

## Task 6: Lookup `<preview>` exposes the embed; composer parse (server + client)

**Files:**
- Modify: `crates/waddle-server/src/server/routes/websocket/handlers/iq/link_preview_lookup.rs`

This makes the composer (pre-send) preview match what gets sent. The token already carries the embed regardless; this is the cosmetic lookup channel.

- [ ] **Step 1: Write the failing test**

In `link_preview_lookup.rs` tests, mirror the existing test that asserts the `<image>` child is emitted in the `<preview>` element. Add:

```rust
#[test]
fn lookup_preview_includes_player_element() {
    // Build a LinkPreviewTokenData with a `player` and run the
    // build_link_preview_lookup_result path used by the existing image test,
    // then assert the serialized <preview> contains a <player> child with the
    // embed url and dimensions. Copy the exact scaffold from the existing
    // "<image> child" test in this module.
}
```

NOTE TO IMPLEMENTER: Copy the existing `<image>`-child lookup test scaffold verbatim, swap to assert a `<player>` child.

- [ ] **Step 2: Run to verify it fails**

Run: `cd server && cargo test -p waddle-server lookup_preview_includes_player_element`
Expected: FAIL — no `<player>` child emitted.

- [ ] **Step 3: Emit `<player>` in `build_link_preview_lookup_result`**

In `build_link_preview_lookup_result` (lines 270–333), after the `if let Some(video) = &data.video { ... }` block, add:

```rust
        if let Some(player) = &data.player {
            let mut player_elem = Element::builder("player", NS_WADDLE_LINK_PREVIEW)
                .attr(xml_ncname!("url").to_owned(), player.url.as_str());
            if let Some(width) = player.width {
                player_elem =
                    player_elem.attr(xml_ncname!("width").to_owned(), width.to_string());
            }
            if let Some(height) = player.height {
                player_elem =
                    player_elem.attr(xml_ncname!("height").to_owned(), height.to_string());
            }
            preview.append_child(player_elem.build());
        }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd server && cargo test -p waddle-server lookup_preview_includes_player_element`
Expected: PASS.

- [ ] **Step 5: Run clippy + commit**

```bash
cd /Users/oyr/projects/waddle
cargo clippy -p waddle-server -- -D warnings
git add server/crates/waddle-server/src/server/routes/websocket/handlers/iq/link_preview_lookup.rs
git commit -m "feat(server): expose player embed in link-preview lookup result"
```

---

## Task 7: Client WASM parses `og:video` into the preview (server crates compiled to WASM)

**Files:**
- Modify: `crates/waddle-xmpp-client/src/messaging/parsing/mod.rs`
- Modify: the `LinkPreviewData` struct definition (search `pub struct LinkPreviewData`)
- Modify: `crates/waddle-xmpp-client-wasm/src/types.rs`
- Modify: `crates/waddle-xmpp-client-wasm/src/conversions.rs`

- [ ] **Step 1: Write the failing parse test**

In `crates/waddle-xmpp-client/src/messaging/parsing/mod.rs` test module (search `fn parse_link_preview` tests, or add a `#[test]`), add:

```rust
#[test]
fn parses_og_video_player_embed_from_xep0511() {
    let el = element(
        r#"<apply-to xmlns="urn:xmpp:fasten:0">
            <rdf:Description xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:og="https://ogp.me/ns#" xmlns:ogv="https://ogp.me/ns#video:" rdf:about="https://www.youtube.com/watch?v=429A_VugWW0">
              <og:title>A video</og:title>
              <og:video>https://www.youtube-nocookie.com/embed/429A_VugWW0</og:video>
              <ogv:type>text/html</ogv:type>
              <ogv:width>1280</ogv:width>
              <ogv:height>720</ogv:height>
            </rdf:Description>
          </apply-to>"#,
    );
    // Use whatever the existing tests use to reach parse_link_preview / the
    // Description element; mirror an existing og:image parse test's harness.
    let preview = parse_link_preview(/* the <rdf:Description> element */, None).expect("preview");
    let player = preview.player_embed.expect("player embed");
    assert_eq!(
        player.url.as_str(),
        "https://www.youtube-nocookie.com/embed/429A_VugWW0"
    );
    assert_eq!(player.width, Some(1280));
    assert_eq!(player.height, Some(720));
}
```

NOTE TO IMPLEMENTER: Read an existing og:image parse test in this module and copy its exact element/harness shape; the snippet above shows the XML to feed in.

- [ ] **Step 2: Run to verify it fails**

Run: `cd server && cargo test -p waddle-xmpp-client parses_og_video_player_embed_from_xep0511`
Expected: FAIL to COMPILE — `LinkPreviewData` has no `player_embed`, `LinkPreviewPlayer` undefined.

- [ ] **Step 3: Add `LinkPreviewPlayer` and the field**

At the `LinkPreviewData` definition, add a sibling struct and field:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkPreviewPlayer {
    pub url: Url,
    pub width: Option<u32>,
    pub height: Option<u32>,
}
```

In `LinkPreviewData`, add after `image`:

```rust
    pub player_embed: Option<LinkPreviewPlayer>,
```

- [ ] **Step 4: Parse it in `parse_link_preview`**

In `parse_link_preview` (lines 371–388), add a parse and set the field in the returned `LinkPreviewData { ... }`:

```rust
    let player_embed = parse_link_preview_player(el);
```

and in the struct literal add `player_embed,`. Add the helper near `parse_link_preview_image`:

```rust
fn parse_link_preview_player(el: &Element) -> Option<LinkPreviewPlayer> {
    let mut url = None;
    let mut secure_url = None;
    let mut is_html = false;
    let mut width = None;
    let mut height = None;
    for child in el.children() {
        if child.ns() == NS_OPENGRAPH && child.name() == "video" {
            url = parse_web_url(child.text().trim());
            continue;
        }
        if child.ns() != NS_OPENGRAPH_VIDEO {
            continue;
        }
        let value = child.text().trim().to_string();
        match child.name() {
            "secure_url" => secure_url = parse_web_url(&value),
            "type" => is_html = value.eq_ignore_ascii_case("text/html"),
            "width" => width = value.parse().ok(),
            "height" => height = value.parse().ok(),
            _ => {}
        }
    }
    let url = secure_url.or(url)?;
    is_html.then_some(LinkPreviewPlayer { url, width, height })
}
```

Add `NS_OPENGRAPH_VIDEO` — reuse the const from `waddle_xmpp::xep::NS_OPENGRAPH_VIDEO` (import it) or define a local `const NS_OPENGRAPH_VIDEO: &str = "https://ogp.me/ns#video:";` next to the existing `NS_OPENGRAPH` used here. Match how `NS_OPENGRAPH` is referenced in this file.

- [ ] **Step 5: Run the parse test to verify it passes**

Run: `cd server && cargo test -p waddle-xmpp-client parses_og_video_player_embed_from_xep0511`
Expected: PASS. Add `player_embed: None` to any other `LinkPreviewData { ... }` literal the compiler flags.

- [ ] **Step 6: Add the WASM-exported type and conversion**

In `crates/waddle-xmpp-client-wasm/src/types.rs`, after `WaddleLinkPreviewImage` (line 82) add:

```rust
#[derive(Debug, Serialize)]
pub struct WaddleLinkPreviewPlayer {
    pub url: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}
```

In `WaddleLinkPreview` (lines 65–73), add after `image`:

```rust
    pub player_embed: Option<WaddleLinkPreviewPlayer>,
```

In `conversions.rs` `link_previews_to_js` (lines 435–453), add after the `image: ...` mapping:

```rust
            player_embed: preview.player_embed.map(|player| WaddleLinkPreviewPlayer {
                url: player.url.to_string(),
                width: player.width,
                height: player.height,
            }),
```

Add `WaddleLinkPreviewPlayer` to the `use` of `types::...` at the top of `conversions.rs`.

- [ ] **Step 7: Build the WASM crate**

Run: `cd server && cargo build -p waddle-xmpp-client-wasm`
Expected: compiles.

- [ ] **Step 8: Run clippy + commit**

```bash
cd /Users/oyr/projects/waddle
cargo clippy -p waddle-xmpp-client -p waddle-xmpp-client-wasm -- -D warnings
git add server/crates/waddle-xmpp-client/src/messaging server/crates/waddle-xmpp-client-wasm/src/types.rs server/crates/waddle-xmpp-client-wasm/src/conversions.rs
git commit -m "feat(server): expose og:video player embed to the chat WASM client"
```

---

## Task 8: Client allowlist mirror (TypeScript)

**Files:**
- Create: `chat/src/lib/xmpp/player-embed-allowlist.ts`
- Test: `chat/tests/player-embed-allowlist.test.ts`

- [ ] **Step 1: Write the failing test**

Create `chat/tests/player-embed-allowlist.test.ts`:

```typescript
import { describe, expect, test } from "bun:test";
import { isAllowedPlayerEmbedOrigin } from "@/lib/xmpp/player-embed-allowlist";

describe("isAllowedPlayerEmbedOrigin", () => {
  test("accepts youtube-nocookie and vimeo player origins", () => {
    expect(isAllowedPlayerEmbedOrigin("https://www.youtube-nocookie.com/embed/429A_VugWW0")).toBe(true);
    expect(isAllowedPlayerEmbedOrigin("https://player.vimeo.com/video/12345")).toBe(true);
  });

  test("rejects non-allowlisted and non-https origins", () => {
    expect(isAllowedPlayerEmbedOrigin("https://evil.example.com/embed/x")).toBe(false);
    expect(isAllowedPlayerEmbedOrigin("https://www.youtube.com/embed/x")).toBe(false); // pre-rewrite host not trusted
    expect(isAllowedPlayerEmbedOrigin("http://www.youtube-nocookie.com/embed/x")).toBe(false);
    expect(isAllowedPlayerEmbedOrigin("not a url")).toBe(false);
  });
});
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd chat && bun test tests/player-embed-allowlist.test.ts`
Expected: FAIL — module not found.

- [ ] **Step 3: Create the module**

Create `chat/src/lib/xmpp/player-embed-allowlist.ts`:

```typescript
// Mirror of the server-side player-embed allowlist (the security boundary lives
// on the server; this is a client-side defense-in-depth re-check before we ever
// emit an <iframe>). Origins here are the FINAL, post-rewrite embed origins —
// the server rewrites youtube.com -> youtube-nocookie.com before sealing, so
// www.youtube.com is intentionally NOT trusted here.
const ALLOWED_PLAYER_EMBED_ORIGINS: readonly string[] = [
  "https://www.youtube-nocookie.com",
  "https://player.vimeo.com",
];

export function isAllowedPlayerEmbedOrigin(url: string): boolean {
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    return false;
  }
  if (parsed.protocol !== "https:") return false;
  return ALLOWED_PLAYER_EMBED_ORIGINS.includes(parsed.origin);
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd chat && bun test tests/player-embed-allowlist.test.ts`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cd /Users/oyr/projects/waddle
git add chat/src/lib/xmpp/player-embed-allowlist.ts chat/tests/player-embed-allowlist.test.ts
git commit -m "feat(chat): add client mirror of the player-embed allowlist"
```

---

## Task 9: Client types + media state (TypeScript)

**Files:**
- Modify: `chat/src/lib/chat-ui.ts`
- Test: `chat/tests/chat-ui-link-preview-media-state.test.ts` (create if absent; otherwise add to the existing chat-ui test)

- [ ] **Step 1: Write the failing test**

Create `chat/tests/chat-ui-link-preview-media-state.test.ts`:

```typescript
import { describe, expect, test } from "bun:test";
import { linkPreviewMediaState, type LinkPreview } from "@/lib/chat-ui";

describe("linkPreviewMediaState player kind", () => {
  test("returns player kind with poster when a playerEmbed is present", () => {
    const preview: LinkPreview = {
      originalUrl: "https://www.youtube.com/watch?v=429A_VugWW0",
      title: "A video",
      image: { url: "https://waddle.example/api/files/x.png", mediaType: "image/png" },
      playerEmbed: { url: "https://www.youtube-nocookie.com/embed/429A_VugWW0", width: 1280, height: 720 },
    };
    const state = linkPreviewMediaState(preview);
    expect(state.kind).toBe("player");
    if (state.kind === "player") {
      expect(state.player.url).toBe("https://www.youtube-nocookie.com/embed/429A_VugWW0");
      expect(state.poster?.url).toBe("https://waddle.example/api/files/x.png");
    }
  });
});
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd chat && bun test tests/chat-ui-link-preview-media-state.test.ts`
Expected: FAIL — `playerEmbed` not on `LinkPreview`; no `"player"` kind.

- [ ] **Step 3: Add the type and field**

In `chat/src/lib/chat-ui.ts`, after `LinkPreviewVideo` (line 67) add:

```typescript
/// Embeddable player iframe (allowlisted og:video) the client renders on click.
export interface LinkPreviewPlayer {
  url: string;
  width?: number;
  height?: number;
}
```

In `LinkPreview` (lines 44–52), add after `video`:

```typescript
  playerEmbed?: LinkPreviewPlayer;
```

- [ ] **Step 4: Add the `"player"` media state**

Replace the `LinkPreviewMediaState` union and `linkPreviewMediaState` (lines 69–82) with:

```typescript
export type LinkPreviewMediaState =
  | { kind: "none" }
  | { kind: "image"; image: LinkPreviewImage }
  | { kind: "video"; video: LinkPreviewVideo; poster?: LinkPreviewImage }
  | { kind: "player"; player: LinkPreviewPlayer; poster?: LinkPreviewImage }
  | { kind: "remote-unavailable" };

export function linkPreviewMediaState(preview: LinkPreview): LinkPreviewMediaState {
  if (preview.video) {
    return { kind: "video", video: preview.video, ...(preview.image ? { poster: preview.image } : {}) };
  }
  if (preview.playerEmbed) {
    return { kind: "player", player: preview.playerEmbed, ...(preview.image ? { poster: preview.image } : {}) };
  }
  if (preview.image) return { kind: "image", image: preview.image };
  if (preview.remoteMediaUnavailable) return { kind: "remote-unavailable" };
  return { kind: "none" };
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cd chat && bun test tests/chat-ui-link-preview-media-state.test.ts`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
cd /Users/oyr/projects/waddle
git add chat/src/lib/chat-ui.ts chat/tests/chat-ui-link-preview-media-state.test.ts
git commit -m "feat(chat): add player media state to link previews"
```

---

## Task 10: Client codec maps the embed with allowlist filter (TypeScript)

**Files:**
- Modify: `chat/src/lib/xmpp/wasm-types.ts`
- Modify: `chat/src/lib/xmpp/wasm-message-codecs.ts`
- Test: `chat/tests/wasm-message-codecs-player.test.ts` (or extend the existing codec test)

- [ ] **Step 1: Write the failing test**

Create `chat/tests/wasm-message-codecs-player.test.ts`. Use the existing codec test as a reference for how to call `linkPreviewFromWasm` (it may be internal — if so, test via the public `messageFromWasm`/whatever the existing codec test imports; mirror that test's import). Minimal direct-shape test:

```typescript
import { describe, expect, test } from "bun:test";
import { linkPreviewFromWasm } from "@/lib/xmpp/wasm-message-codecs";

describe("linkPreviewFromWasm player embed", () => {
  test("maps an allowlisted player embed", () => {
    const preview = linkPreviewFromWasm({
      original_url: "https://www.youtube.com/watch?v=429A_VugWW0",
      player_embed: { url: "https://www.youtube-nocookie.com/embed/429A_VugWW0", width: 1280, height: 720 },
    });
    expect(preview.playerEmbed?.url).toBe("https://www.youtube-nocookie.com/embed/429A_VugWW0");
  });

  test("drops a non-allowlisted player embed", () => {
    const preview = linkPreviewFromWasm({
      original_url: "https://example.com/x",
      player_embed: { url: "https://evil.example.com/embed/x" },
    });
    expect(preview.playerEmbed).toBeUndefined();
  });
});
```

NOTE: If `linkPreviewFromWasm` is not exported, add `export` to it (it is currently a module-private `function`). That is a safe, minimal change.

- [ ] **Step 2: Run to verify it fails**

Run: `cd chat && bun test tests/wasm-message-codecs-player.test.ts`
Expected: FAIL — `player_embed` not in `WasmLinkPreview`; `playerEmbed` not mapped.

- [ ] **Step 3: Add the WASM type**

In `chat/src/lib/xmpp/wasm-types.ts`, after `WasmLinkPreviewImage` (ends line 163) add:

```typescript
export interface WasmLinkPreviewPlayer {
  url: string;
  width?: number | null;
  height?: number | null;
}
```

In `WasmLinkPreview` (lines 148–155), add after `image`:

```typescript
  player_embed?: WasmLinkPreviewPlayer | null;
```

- [ ] **Step 4: Map it (with the allowlist filter) in `linkPreviewFromWasm`**

In `chat/src/lib/xmpp/wasm-message-codecs.ts`, import the allowlist at the top:

```typescript
import { isAllowedPlayerEmbedOrigin } from "@/lib/xmpp/player-embed-allowlist";
```

In `linkPreviewFromWasm` (lines 161–180), before the `return { ... }`, add:

```typescript
  const playerEmbed = preview.player_embed && isAllowedPlayerEmbedOrigin(preview.player_embed.url)
    ? preview.player_embed
    : undefined;
```

In the returned object literal, add (after the `image` spread):

```typescript
    ...(playerEmbed
      ? {
          playerEmbed: {
            url: playerEmbed.url,
            ...(typeof playerEmbed.width === "number" ? { width: playerEmbed.width } : {}),
            ...(typeof playerEmbed.height === "number" ? { height: playerEmbed.height } : {}),
          },
        }
      : {}),
```

Ensure `linkPreviewFromWasm` is `export`ed (add `export` keyword if missing).

- [ ] **Step 5: Run test to verify it passes**

Run: `cd chat && bun test tests/wasm-message-codecs-player.test.ts`
Expected: both PASS.

- [ ] **Step 6: Commit**

```bash
cd /Users/oyr/projects/waddle
git add chat/src/lib/xmpp/wasm-types.ts chat/src/lib/xmpp/wasm-message-codecs.ts chat/tests/wasm-message-codecs-player.test.ts
git commit -m "feat(chat): decode allowlisted player embed from wasm preview"
```

---

## Task 11: Composer lookup parses `<player>` (TypeScript)

**Files:**
- Modify: `chat/src/lib/xmpp/link-preview.ts`
- Test: extend `chat/tests/link-preview.test.ts`

This keeps the composer preview consistent with the sent message. The result type `LinkPreviewLookupReadyResult` gains an optional `playerEmbed`.

- [ ] **Step 1: Write the failing test**

In `chat/tests/link-preview.test.ts`, mirror the existing test that asserts `<image>` parsing, adding a `<player>` child to the lookup XML and asserting `result.playerEmbed`. Use the existing test's `parseLookupResponse` access pattern (it may test via the public lookup function). Assert:

```typescript
// after parsing a ready <preview> containing
//   <player xmlns="urn:waddle:link-preview:0"
//     url="https://www.youtube-nocookie.com/embed/429A_VugWW0" width="1280" height="720"/>
expect(result.playerEmbed?.url).toBe("https://www.youtube-nocookie.com/embed/429A_VugWW0");
expect(result.playerEmbed?.width).toBe(1280);
```

NOTE TO IMPLEMENTER: copy the existing `<image>` lookup test's exact harness.

- [ ] **Step 2: Run to verify it fails**

Run: `cd chat && bun test tests/link-preview.test.ts`
Expected: FAIL — `playerEmbed` not parsed.

- [ ] **Step 3: Add the parse helper and wire it in**

In `chat/src/lib/xmpp/link-preview.ts`, add a helper next to `previewImage` (lines 196–218):

```typescript
function previewPlayer(parent: Element): Partial<LinkPreviewLookupReadyResult> {
  const player = Array.from(parent.children).find(
    (child) => child.localName === "player" && child.namespaceURI === NS_WADDLE_LINK_PREVIEW,
  );
  if (!player) return {};
  const url = player.getAttribute("url")?.trim();
  if (!url || !isAllowedPlayerEmbedOrigin(url)) return {};
  const width = optionalPositiveInteger(player.getAttribute("width"));
  const height = optionalPositiveInteger(player.getAttribute("height"));
  return {
    playerEmbed: {
      url,
      ...(width ? { width } : {}),
      ...(height ? { height } : {}),
    },
  };
}
```

Import the allowlist at the top:

```typescript
import { isAllowedPlayerEmbedOrigin } from "@/lib/xmpp/player-embed-allowlist";
```

In `parseLookupResponse` (lines 76–107), in the returned ready object, add after `...previewImage(preview, trustedMediaOrigin),`:

```typescript
    ...previewPlayer(preview),
```

Add `playerEmbed?: LinkPreviewPlayer` to the `LinkPreviewLookupReadyResult` type (search its definition in this file or a sibling types file) and import `LinkPreviewPlayer` from `@/lib/chat-ui`.

- [ ] **Step 4: Ensure the composer carries `playerEmbed` into the displayed preview**

Find where a `LinkPreviewLookupReadyResult` is turned into a `LinkPreview` for the composer's local preview (search for `originalUrl:` assignments consuming the lookup result, likely in `link-preview-composer.ts`). Map `playerEmbed` through. If the composer only stores the token and re-renders from the sent message, this step is a no-op — verify and note.

- [ ] **Step 5: Run test to verify it passes**

Run: `cd chat && bun test tests/link-preview.test.ts`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
cd /Users/oyr/projects/waddle
git add chat/src/lib/xmpp/link-preview.ts chat/src/lib/xmpp/link-preview-composer.ts chat/tests/link-preview.test.ts
git commit -m "feat(chat): parse player embed from lookup result"
```

---

## Task 12: Render the player card with click-to-load iframe (Vue)

**Files:**
- Modify: `chat/src/components/chat/MessageBody.vue`
- Test: extend `chat/tests/message-body-link-previews.test.ts`

- [ ] **Step 1: Write the failing tests**

In `chat/tests/message-body-link-previews.test.ts`, add (mirror the existing direct-video render test at lines 46–65):

```typescript
test("renders a player embed as a play control without loading the iframe", async () => {
  const html = await renderMessageBody({
    message: messageWithPreviews([
      {
        originalUrl: "https://www.youtube.com/watch?v=429A_VugWW0",
        normalizedUrl: "https://www.youtube.com/watch?v=429A_VugWW0",
        title: "A video",
        image: { url: "https://waddle.example/api/files/x.png", mediaType: "image/png" },
        playerEmbed: { url: "https://www.youtube-nocookie.com/embed/429A_VugWW0", width: 1280, height: 720 },
      },
    ]),
  });

  // Accessible play control exists; the iframe (and its network fetch) is NOT
  // present until the user clicks play.
  expect(html).toContain('aria-label="Play video: A video"');
  expect(html).not.toContain("<iframe");
  expect(html).not.toContain("youtube-nocookie.com/embed/429A_VugWW0");
  expect(html).toContain("A video");
});

test("does not render a player card for a non-allowlisted embed", async () => {
  // playerEmbed is dropped upstream by the codec; a preview that still somehow
  // carries a bad origin must not produce an iframe. Construct the preview
  // directly to exercise the component's own origin re-check.
  const html = await renderMessageBody({
    message: messageWithPreviews([
      {
        originalUrl: "https://example.com/x",
        title: "Bad",
        playerEmbed: { url: "https://evil.example.com/embed/x" },
      },
    ]),
  });
  expect(html).not.toContain("aria-label=\"Play video");
  expect(html).not.toContain("<iframe");
});
```

- [ ] **Step 2: Run to verify they fail**

Run: `cd chat && bun test tests/message-body-link-previews.test.ts`
Expected: FAIL — no player card rendered.

- [ ] **Step 3: Add script-setup state for player cards**

In `MessageBody.vue` `<script setup>`, import the allowlist:

```typescript
import { isAllowedPlayerEmbedOrigin } from "@/lib/xmpp/player-embed-allowlist";
```

Update `linkInfoCards` (currently `filter((card) => card.mediaState.kind !== "video")`) to also exclude player:

```typescript
const linkInfoCards = computed(() =>
  linkPreviewCards.value.filter(
    (card) => card.mediaState.kind !== "video" && card.mediaState.kind !== "player",
  ),
);
```

Add player-card computed + playback state next to `videoPreviewCards`/`playingVideos`:

```typescript
const playerPreviewCards = computed(() =>
  linkPreviews.value.flatMap((preview) => {
    const state = linkPreviewMediaState(preview);
    // Defense in depth: only ever iframe an allowlisted embed origin, even
    // though the codec already filtered it.
    if (state.kind !== "player" || !isAllowedPlayerEmbedOrigin(state.player.url)) return [];
    return [{ preview, player: state.player, ...(state.poster ? { poster: state.poster } : {}) }];
  }),
);
const playingEmbeds = reactive(new Set<string>());
function startEmbedPlayback(preview: MessageLinkPreview): void {
  playingEmbeds.add(videoCardKey(preview));
}
```

(`videoCardKey` already exists and is reused for the embed key — preview is never both video and player.)

- [ ] **Step 4: Add the template block**

In the template, after the `videoPreviewCards` `<div v-for>` block (ends line 341), add:

```vue
    <!-- Embeddable player previews (allowlisted og:video). Loads the third-party
         iframe only after the user clicks play; the facade shows the
         Waddle-proxied thumbnail and nothing contacts the provider until then. -->
    <div
      v-for="card in playerPreviewCards"
      :key="`player:${videoCardKey(card.preview)}`"
      class="flex max-w-xl flex-col gap-1 rounded-md border border-border bg-muted/40 p-3 text-left"
    >
      <span class="type-meta flex items-center gap-1 text-muted-foreground">
        <ExternalLink aria-hidden="true" class="h-3.5 w-3.5" />
        {{ linkPreviewHost(card.preview.originalUrl) }}
      </span>
      <iframe
        v-if="playingEmbeds.has(videoCardKey(card.preview))"
        :src="card.player.url"
        :title="card.preview.title ?? 'Embedded player'"
        class="aspect-video w-full rounded border border-border bg-black"
        loading="lazy"
        allow="encrypted-media; picture-in-picture; fullscreen"
        allowfullscreen
        referrerpolicy="strict-origin-when-cross-origin"
      />
      <button
        v-else
        type="button"
        class="relative flex aspect-video w-full items-center justify-center overflow-hidden rounded border border-border bg-black/80 text-white transition-colors hover:bg-black"
        :aria-label="`Play video${card.preview.title ? ': ' + card.preview.title : ''}`"
        @click.stop.prevent="startEmbedPlayback(card.preview)"
      >
        <img
          v-if="card.poster"
          :src="card.poster.url"
          :alt="card.poster.alt ?? ''"
          loading="lazy"
          decoding="async"
          class="absolute inset-0 h-full w-full object-cover"
        />
        <Play aria-hidden="true" class="relative h-10 w-10 drop-shadow" />
      </button>
      <a
        :href="linkPreviewHref(card.preview.originalUrl) ?? undefined"
        target="_blank"
        rel="noopener noreferrer"
        class="type-field text-foreground hover:underline"
        @click.stop
      >{{ card.preview.title ?? card.preview.originalUrl }}</a>
      <span v-if="card.preview.description" class="type-caption line-clamp-2 text-muted-foreground">{{ card.preview.description }}</span>
    </div>
```

(`ExternalLink`, `Play`, `linkPreviewHost`, `linkPreviewHref`, `videoCardKey` are already imported/defined for the video card.)

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd chat && bun test tests/message-body-link-previews.test.ts`
Expected: all PASS (including pre-existing ones).

- [ ] **Step 6: Commit**

```bash
cd /Users/oyr/projects/waddle
git add chat/src/components/chat/MessageBody.vue chat/tests/message-body-link-previews.test.ts
git commit -m "feat(chat): render click-to-load embedded player previews"
```

---

## Task 13: Full-suite verification

- [ ] **Step 1: Server tests + clippy**

Run: `cd server && cargo test -p waddle-server -p waddle-xmpp -p waddle-xmpp-client -p waddle-xmpp-client-wasm`
Expected: all PASS.

Run: `cd server && cargo clippy --workspace -- -D warnings`
Expected: no warnings.

- [ ] **Step 2: Chat tests + lint (knip hard rule)**

Run: `cd chat && bun test`
Expected: all PASS.

Run: `cd chat && bun run lint`
Expected: knip clean — no unused files/exports/deps. (The new files are all wired up; `player-embed-allowlist.ts` is imported by the codec, the component, and the lookup parser.)

- [ ] **Step 3: Build the chat WASM artifact and confirm the new field surfaces**

If the chat build regenerates WASM bindings from the Rust client crate, run the project's WASM build (search `package.json`/`env.cue` for the wasm build task, e.g. `bun run build:wasm` or a cuenv task) so `player_embed` reaches the JS `WasmLinkPreview`. Confirm `chat`'s type for the generated preview includes the field, or that the hand-written `wasm-types.ts` is the source of truth (it is, per Task 10).

- [ ] **Step 4: Manual smoke (optional, via /run)**

Paste `https://www.youtube.com/watch?v=429A_VugWW0&list=RD429A_VugWW0` in a room; confirm the card shows the thumbnail + play overlay, and clicking loads the `youtube-nocookie` iframe.

- [ ] **Step 5: Update the PR**

```bash
cd /Users/oyr/projects/waddle
git push
gh pr ready 857
```

Update the PR description to reflect the completed work (remove draft once green), then monitor CI per project rules.

---

## Notes for the implementer

- **Mutual exclusivity:** a preview is either a direct video (XEP-0447 file-share) or a player embed (XEP-0511 og:video), never both. The server enforces this because direct video only comes from a direct-media URL and the player only from an HTML page.
- **Why two Rust og:video parsers:** the server stamps via `xep0511::build_link_metadata_element`; the chat client reads via `waddle-xmpp-client::parse_link_preview` (its own og parser). Both must handle og:video — Task 4 (build/parse in xep0511) and Task 7 (parse in the client crate).
- **CSP:** there is no app-level CSP today. If one is added later it MUST include `frame-src https://www.youtube-nocookie.com https://player.vimeo.com`. Add a code comment near the allowlist if you touch deployment headers.
- **`list=` param:** dropped — the embed URL comes from the page's own `og:video:secure_url`, which is the single-video embed. No special handling needed.

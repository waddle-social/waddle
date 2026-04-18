# Gap Analysis: Apple App vs Web Chat

> Generated: 2026-04-18
> Last updated: 2026-04-19 (loop 3, items 5.1/6.1)

## Legend

- **[CRITICAL]** - Core messaging functionality users expect
- **[HIGH]** - Important features that significantly impact UX
- **[MEDIUM]** - Nice-to-have features that improve experience
- **[LOW]** - Polish and parity features

## Status Key

- [ ] Not started
- [~] In progress
- [x] Complete

---

## 1. Messaging Features

| # | Gap | Priority | Status | XEP | Notes |
|---|-----|----------|--------|-----|-------|
| 1.1 | Message replies (quote-reply) | CRITICAL | [x] | XEP-0461, XEP-0428 | Reply stanza construction/parsing, fallback stripping, reply indicator UI, context menu reply action, composer reply preview |
| 1.2 | Rich text markup (bold, italic, code, etc.) | CRITICAL | [x] | XEP-0394 | Parse markup spans from stanzas, render with AttributedString (bold, italic, strikethrough, code, links), rebase offsets for reply fallback |
| 1.3 | @mentions with autocomplete | CRITICAL | [ ] | XEP-0372 | Web has inline autocomplete in editor |
| 1.4 | Broadcast mentions (@everyone, @here) | HIGH | [ ] | XEP-0513 | Web supports both broadcast types |
| 1.5 | Read receipts / chat markers | HIGH | [x] | XEP-0333 | Send displayed markers for incoming messages, parse displayed marker stanzas |
| 1.6 | Typing indicators (chat state) | HIGH | [x] | XEP-0085 | Send composing/paused on text input, parse incoming chat state, typing indicator UI with auto-expiry |
| 1.7 | Message moderation (admin retract) | MEDIUM | [ ] | XEP-0425 | Web supports moderation with reason field |
| 1.8 | Delivery status echo matching | LOW | [~] | - | Apple has states but web has body-fallback matching + pendingEcho tracking |

## 2. Direct Messages

| # | Gap | Priority | Status | XEP | Notes |
|---|-----|----------|--------|-----|-------|
| 2.1 | 1-on-1 DM conversations | CRITICAL | [ ] | type="chat" | Web has full DM system with presence, unread counts, MAM |
| 2.2 | DM conversation list panel | CRITICAL | [ ] | - | Web has DmPanel with last message preview, timestamps |
| 2.3 | DM presence tracking | HIGH | [ ] | - | Web shows online/away/dnd/offline per DM peer |
| 2.4 | New DM dialog (user search) | HIGH | [ ] | - | Web has user search + DM initiation |
| 2.5 | DM unread counts | MEDIUM | [ ] | - | Web tracks unread per conversation |

## 3. Threading & Forums

| # | Gap | Priority | Status | XEP | Notes |
|---|-----|----------|--------|-----|-------|
| 3.1 | Forum channel topic creation | CRITICAL | [ ] | XEP-0508 | Web creates topics with threadCreate element |
| 3.2 | Forum thread replies | CRITICAL | [ ] | XEP-0508, XEP-0201 | Web has full thread reply composition |
| 3.3 | Thread panel / viewer | HIGH | [ ] | - | Web has ThreadPanel with breadcrumb navigation |
| 3.4 | Nested thread navigation | MEDIUM | [ ] | - | Web supports thread stack via URL query params |

## 4. Media & Files

| # | Gap | Priority | Status | XEP | Notes |
|---|-----|----------|--------|-----|-------|
| 4.1 | File upload (images, documents) | CRITICAL | [ ] | XEP-0363, XEP-0447 | Web has drag-drop, paste, file picker (10MB max) |
| 4.2 | Inline image display | CRITICAL | [ ] | XEP-0446 | Web renders images inline with dimensions |
| 4.3 | Image lightbox viewer | HIGH | [ ] | - | Web has full-screen image viewer |
| 4.4 | GIF picker (GIPHY) | MEDIUM | [ ] | - | Web has trending + search GIF integration |
| 4.5 | Sticker support | LOW | [ ] | XEP-0449 | Web supports sticker packs |
| 4.6 | File download | HIGH | [ ] | - | Web has download button for non-image attachments |

## 5. Emoji

| # | Gap | Priority | Status | XEP | Notes |
|---|-----|----------|--------|-----|-------|
| 5.1 | Emoji picker UI | HIGH | [x] | - | Emoji picker popover with categorized grid (smileys, reactions, objects, nature), search field, inserts into composer |
| 5.2 | Emoji autocomplete in composer | MEDIUM | [ ] | - | Web has `:emoji_name:` autocomplete |

## 6. Channel & Waddle Management

| # | Gap | Priority | Status | XEP | Notes |
|---|-----|----------|--------|-----|-------|
| 6.1 | Create channel (text/forum) | HIGH | [x] | XEP-0050 | Two-step ad-hoc command flow, form with name/description/type/position, create button in channel rail |
| 6.2 | Edit channel (name, description) | MEDIUM | [ ] | - | Web has EditChannelDialog |
| 6.3 | Waddle settings dialog | MEDIUM | [ ] | - | Web has WaddleSettingsDialog |
| 6.4 | Delete waddle | LOW | [ ] | - | Web has delete with confirmation |

## 7. Member Management

| # | Gap | Priority | Status | XEP | Notes |
|---|-----|----------|--------|-----|-------|
| 7.1 | Add member to waddle | HIGH | [ ] | - | Web has member search + add via REST API |
| 7.2 | Remove member from waddle | HIGH | [ ] | - | Web has remove via REST API |
| 7.3 | Change member role | MEDIUM | [ ] | - | Web supports owner/admin/moderator/member roles |
| 7.4 | Hat/badge display (role badges) | MEDIUM | [ ] | XEP-0317 | Web shows colored role badges on messages |

## 8. Notifications

| # | Gap | Priority | Status | XEP | Notes |
|---|-----|----------|--------|-----|-------|
| 8.1 | Push notifications | HIGH | [ ] | XEP-0357 | Web has VAPID-based push via service worker |
| 8.2 | In-app notification toasts | MEDIUM | [ ] | - | Web shows on @mention with auto-dismiss |

## 9. Rich Text Editor

| # | Gap | Priority | Status | XEP | Notes |
|---|-----|----------|--------|-----|-------|
| 9.1 | Rich text composer (not plain text) | HIGH | [ ] | - | Web uses TipTap/ProseMirror editor |
| 9.2 | Link auto-detection | MEDIUM | [ ] | - | Web auto-detects URLs in editor |
| 9.3 | Code block syntax highlighting | LOW | [ ] | - | Web uses Shiki for code coloring |

## 10. Connection & Reliability

| # | Gap | Priority | Status | XEP | Notes |
|---|-----|----------|--------|-----|-------|
| 10.1 | Stream Management (session resumption) | HIGH | [ ] | XEP-0198 | Web resumes sessions, detects fresh-bind for re-sync |
| 10.2 | Auto-reconnect with state recovery | HIGH | [ ] | - | Web handles reconnection with MAM re-sync |

## 11. Code Quality / CLAUDE.md Compliance

| # | Gap | Priority | Status | Notes |
|---|-----|----------|--------|-------|
| 11.1 | XML string concatenation | CRITICAL | [ ] | Apple app builds XML via string formatting in XMPPXML.swift — violates CLAUDE.md rule: "Never construct XML with format!, string concatenation" |

---

## Summary

| Priority | Count | Description |
|----------|-------|-------------|
| CRITICAL | 9 | Core messaging gaps that block feature parity |
| HIGH | 15 | Important UX gaps |
| MEDIUM | 11 | Enhancement gaps |
| LOW | 4 | Polish gaps |
| **Total** | **39** | |

## Recommended Priority Order

1. **Message replies** (1.1) - fundamental chat feature
2. **DM conversations** (2.1, 2.2) - users expect private messaging
3. **File upload & image display** (4.1, 4.2) - media sharing is essential
4. **Rich text markup** (1.2) - messages look plain without formatting
5. **Forum/thread support** (3.1, 3.2) - channel type already exists but non-functional
6. **@mentions** (1.3) - critical for group chat usability
7. **Read receipts & typing** (1.5, 1.6) - real-time feedback
8. **Emoji picker** (5.1) - reactions exist but no easy way to pick emoji
9. **Channel/member management** (6.1, 7.1, 7.2) - admin functionality
10. **Push notifications** (8.1) - mobile users need push
11. **Stream management** (10.1) - reliability on mobile networks
12. **XML construction refactor** (11.1) - code quality compliance

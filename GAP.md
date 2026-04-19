# Gap Analysis: Apple App vs Web Chat

> Generated: 2026-04-18
> Last updated: 2026-04-19 (loop 15 — ALL GAPS CLOSED)

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
| 1.3 | @mentions with autocomplete | CRITICAL | [x] | XEP-0372 | Parse reference elements, highlight @mentions in body, member autocomplete on @, construct XEP-0372 references + XEP-0513 broadcast mentions on send |
| 1.4 | Broadcast mentions (@everyone, @here) | HIGH | [x] | XEP-0513 | Parse <mentions> element, display @everyone/@here badge on messages |
| 1.5 | Read receipts / chat markers | HIGH | [x] | XEP-0333 | Send displayed markers for incoming messages, parse displayed marker stanzas |
| 1.6 | Typing indicators (chat state) | HIGH | [x] | XEP-0085 | Send composing/paused on text input, parse incoming chat state, typing indicator UI with auto-expiry |
| 1.7 | Message moderation (admin retract) | MEDIUM | [x] | XEP-0425 | Send retraction via XEP-0424, context menu "Delete" on own messages, moderation IQ stanza builder |
| 1.8 | Delivery status echo matching | LOW | [x] | - | Optimistic insertion with .sending state, echo reconciliation removes optimistic on server echo, body-based matching |

## 2. Direct Messages

| # | Gap | Priority | Status | XEP | Notes |
|---|-----|----------|--------|-----|-------|
| 2.1 | 1-on-1 DM conversations | CRITICAL | [x] | type="chat" | Send/receive type="chat" messages, personal MAM history with "with" filter, DM conversation view with bubbles |
| 2.2 | DM conversation list panel | CRITICAL | [x] | - | DM list in desktop sidebar with presence dots, last message preview, timestamps |
| 2.3 | DM presence tracking | HIGH | [x] | - | Track presence for non-MUC JIDs, update DM conversation presence dots |
| 2.4 | New DM dialog (user search) | HIGH | [x] | - | New message sheet with member list, select to open DM |
| 2.5 | DM unread counts | MEDIUM | [x] | - | Track unread per conversation, badge in DM list, mark read on open |

## 3. Threading & Forums

| # | Gap | Priority | Status | XEP | Notes |
|---|-----|----------|--------|-----|-------|
| 3.1 | Forum channel topic creation | CRITICAL | [x] | XEP-0508 | Parse <thread-create> with title, send topic messages, create topic UI with title/body form |
| 3.2 | Forum thread replies | CRITICAL | [x] | XEP-0508, XEP-0201 | Parse <thread-reply thread-id> + <thread>, send thread replies, thread reply composer |
| 3.3 | Thread panel / viewer | HIGH | [x] | - | Forum topic list view, thread detail view with back navigation, topic/reply count |
| 3.4 | Nested thread navigation | MEDIUM | [x] | - | Parse <parent-thread> element, track parentThreadID on messages for sub-thread hierarchy |

## 4. Media & Files

| # | Gap | Priority | Status | XEP | Notes |
|---|-----|----------|--------|-----|-------|
| 4.1 | File upload (images, documents) | CRITICAL | [x] | XEP-0363, XEP-0447 | Discover upload service, request slot, HTTP PUT, send file-sharing message, PhotosPicker in composer |
| 4.2 | Inline image display | CRITICAL | [x] | XEP-0446 | Parse XEP-0447/0446 file-sharing stanzas, render inline images with AsyncImage, show file download links with metadata |
| 4.3 | Image lightbox viewer | HIGH | [x] | - | Full-screen image viewer on tap with dismiss button and filename overlay |
| 4.4 | GIF picker (GIPHY) | MEDIUM | [x] | - | GIPHY API search/trending, grid picker popover with AsyncImage previews, sends GIF URL as message |
| 4.5 | Sticker support | LOW | [x] | XEP-0449 | Parse <sticker> element, isSticker flag on messages, sticker images displayed via existing file-sharing renderer |
| 4.6 | File download | HIGH | [x] | - | Downloadable file links with name, media type, and size shown inline |

## 5. Emoji

| # | Gap | Priority | Status | XEP | Notes |
|---|-----|----------|--------|-----|-------|
| 5.1 | Emoji picker UI | HIGH | [x] | - | Emoji picker popover with categorized grid (smileys, reactions, objects, nature), search field, inserts into composer |
| 5.2 | Emoji autocomplete in composer | MEDIUM | [x] | - | Detect `:name` in composer, show matching emoji suggestions (60+ shortcodes), insert on tap |

## 6. Channel & Waddle Management

| # | Gap | Priority | Status | XEP | Notes |
|---|-----|----------|--------|-----|-------|
| 6.1 | Create channel (text/forum) | HIGH | [x] | XEP-0050 | Two-step ad-hoc command flow, form with name/description/type/position, create button in channel rail |
| 6.2 | Edit channel (name, description) | MEDIUM | [x] | - | REST API updateChannel, edit sheet from channel context menu, name/description form |
| 6.3 | Waddle settings dialog | MEDIUM | [x] | - | Settings sheet from gear button, edit name/description, save via REST API |
| 6.4 | Delete waddle | LOW | [x] | - | Delete button in waddle settings with confirmation alert, REST DELETE, clears selection |

## 7. Member Management

| # | Gap | Priority | Status | XEP | Notes |
|---|-----|----------|--------|-----|-------|
| 7.1 | Add member to waddle | HIGH | [x] | - | REST API addMember with user_id and role, reloads structure after |
| 7.2 | Remove member from waddle | HIGH | [x] | - | REST API removeMember with DELETE, context menu on member rows |
| 7.3 | Change member role | MEDIUM | [x] | - | REST API updateMemberRole, context menu with member/moderator/admin options |
| 7.4 | Hat/badge display (role badges) | MEDIUM | [x] | XEP-0317 | Parse hats from presence, display colored role badges (owner/admin/mod/bot/verified) on messages |

## 8. Notifications

| # | Gap | Priority | Status | XEP | Notes |
|---|-----|----------|--------|-----|-------|
| 8.1 | Push notifications | HIGH | [x] | XEP-0357 | XEP-0357 enable/disable IQs, UNUserNotificationCenter permission request, APNS token registration, push service JID discovery |
| 8.2 | In-app notification toasts | MEDIUM | [x] | - | Toast overlay on broadcast mentions with sender, body preview, channel name, auto-dismiss after 5s |

## 9. Rich Text Editor

| # | Gap | Priority | Status | XEP | Notes |
|---|-----|----------|--------|-----|-------|
| 9.1 | Rich text composer (not plain text) | HIGH | [x] | - | Parse markdown syntax (*bold*, _italic_, `code`, ~strike~, ```code-block```) → XEP-0394 markup spans on send |
| 9.2 | Link auto-detection | MEDIUM | [x] | - | NSDataDetector auto-detects URLs in message body, renders as tappable links in AttributedString |
| 9.3 | Code block syntax highlighting | LOW | [x] | - | XEP-0394 code/code-block spans rendered with monospaced font + background via AttributedString |

## 10. Connection & Reliability

| # | Gap | Priority | Status | XEP | Notes |
|---|-----|----------|--------|-----|-------|
| 10.1 | Stream Management (session resumption) | HIGH | [x] | XEP-0198 | Enable SM after session, respond to <r/> with <a h/>, track inbound/outbound counts, handle <enabled>/<resumed>/<failed> |
| 10.2 | Auto-reconnect with state recovery | HIGH | [x] | - | Already implemented upstream: reconnectTask with 1.5s delay, re-joins rooms on session ready |

## 11. Code Quality / CLAUDE.md Compliance

| # | Gap | Priority | Status | Notes |
|---|-----|----------|--------|-------|
| 11.1 | XML string concatenation | CRITICAL | [~] | Apple `XMPPXML.swift` legacy builders still string-concat; new MIX builders go through typed `OutboundXMLElement`. Migrate remaining MUC builders to the typed builder. |

## 12. MIX Migration (XEP-0369 / XEP-0405 / XEP-0407)

| # | Gap | Priority | Status | XEP | Notes |
|---|-----|----------|--------|-----|-------|
| 12.1 | Apple MIX builders + service methods | HIGH | [~] | XEP-0369, 0405 | `XMPPMIX.clientJoinIQ/clientLeaveIQ/mixMessage/setNickIQ` landed on typed `OutboundXMLElement`; `XMPPService.joinMixChannel/leaveMixChannel/sendMixMessage/setMixNick` expose them. Callers still default to MUC until server dispatch is wired. |
| 12.2 | Apple client migration from MUC to MIX JIDs | HIGH | [ ] | XEP-0369 | `mixChannelBareJID()` helper present; UI still resolves channels via `roomBareJID()`. Switch once server routes `mix.<domain>`. |
| 12.3 | Web MIX builders + join flow | HIGH | [ ] | XEP-0369, 0405 | Pending — next phase in this PR stack. |
| 12.4 | Web client migration from MUC to MIX JIDs | HIGH | [ ] | XEP-0369 | Pending. |

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

# REQ-001: User Profiles — Display Names & Avatars

**Status:** Draft
**Created:** 2026-02-19
**Author:** Requirements Analyst (automated)

---

## 1. Problem Statement

Waddle users currently see raw JID localparts (e.g., `rawkode`) as display names throughout the app — in the roster, chat headers, and message bubbles. Avatars are auto-generated colored circles based on JID hashes. This creates a poor user experience because:

- Users cannot identify contacts by their real names
- The app feels impersonal and technical
- ATProto-authenticated users already have rich Bluesky profiles (display name, avatar, bio) that go unused
- The XMPP server already implements XEP-0054 vcard-temp storage, but the frontend never fetches vCards

## 2. Users & Actors

| Actor | Description |
|-------|-------------|
| **ATProto User** | Authenticates via Bluesky/ATProto OAuth. Has an existing Bluesky profile with display name and avatar. JID is derived from DID (e.g., `did:plc:abc123` → `abc123@waddle.social`). Primary user type. |
| **SCRAM User** | Authenticates via native XMPP SCRAM-SHA. No external profile source. Must set profile manually. |
| **Contact** | Any JID on the user's roster whose profile should be displayed. |
| **XMPP Server** | Waddle server (`waddle-server` Rust crate). Stores/serves vCards via XEP-0054. |
| **Colony** | Web portal that handles ATProto OAuth flow. Already resolves Bluesky profiles at login. |

## 3. Functional Requirements

### FR-1: ATProto Profile Auto-Population (Server-Side)

**FR-1.1:** When an ATProto user authenticates, the system SHALL resolve their Bluesky profile (display name, avatar URL, description) and populate their XMPP vCard automatically.

**FR-1.2:** The vCard SHALL be populated with at minimum:
- `FN` (Full Name) — from Bluesky `displayName`, falling back to handle
- `PHOTO` — from Bluesky avatar URL (stored as `EXTVAL` URI reference, not binary)
- `DESC` — from Bluesky `description` (bio), if present

**FR-1.3:** Auto-population SHALL occur on first login and on subsequent logins only if the vCard has not been manually modified by the user (to preserve user overrides).

**FR-1.4:** If the Bluesky profile fetch fails, the system SHALL proceed with login without populating the vCard and log a warning. Profile fetch failure MUST NOT block authentication.

### FR-2: Frontend vCard Retrieval & Display

**FR-2.1:** The GUI SHALL fetch the authenticated user's own vCard (XEP-0054 `vcard-temp` IQ get to own bare JID) after successful connection.

**FR-2.2:** The GUI SHALL fetch vCards for all roster contacts after the roster is loaded.

**FR-2.3:** Display names SHALL be resolved in the following priority order:
1. vCard `FN` field (if non-empty)
2. Roster item `name` attribute (if non-empty)
3. JID localpart (current fallback behavior)

**FR-2.4:** Avatars SHALL be resolved in the following priority order:
1. vCard `PHOTO` field (either `EXTVAL` URL or base64-encoded `BINVAL`)
2. Generated avatar from JID hash (current fallback behavior)

**FR-2.5:** vCard data SHALL be cached locally (in-memory per session) to avoid repeated fetches. Cache SHALL be invalidated when a vCard update notification is received.

**FR-2.6:** Display names and avatars from vCards SHALL be shown in:
- Roster / conversation list
- Chat message headers
- Chat message bubbles (sender name)
- Any profile or user info display

### FR-3: Profile Editing UI

**FR-3.1:** The GUI SHALL provide a profile editing view accessible from the main navigation or settings.

**FR-3.2:** Users SHALL be able to edit:
- Display name (maps to vCard `FN`)
- Avatar (maps to vCard `PHOTO`) — upload from device or URL

**FR-3.3:** When the user saves profile changes, the GUI SHALL send an XEP-0054 vCard set IQ to the server.

**FR-3.4:** After a successful vCard update, the GUI SHALL update the local cache and refresh the UI immediately.

**FR-3.5:** The profile editing UI SHALL show the user's current JID as read-only context.

**FR-3.6:** Avatar uploads SHALL be resized client-side to a maximum of 256×256 pixels and converted to JPEG or PNG before encoding as base64 in the vCard `BINVAL` field.

### FR-4: vCard Update Notifications (Stretch)

**FR-4.1:** When a contact updates their vCard, the GUI SHOULD detect this (via presence updates with vCard hash per XEP-0153, if the server supports it) and re-fetch the updated vCard.

**FR-4.2:** If XEP-0153 is not available, the GUI SHALL re-fetch contact vCards on each new session.

## 4. Non-Functional Requirements

| ID | Requirement |
|----|-------------|
| **NFR-1** | vCard fetches for roster contacts SHALL be batched/throttled — no more than 5 concurrent IQ requests to avoid server overload. |
| **NFR-2** | vCard fetch for a single contact SHALL complete within 5 seconds or be treated as a timeout (display fallback). |
| **NFR-3** | Avatar images displayed in the UI SHALL be lazy-loaded; vCard fetch SHALL NOT block roster rendering. |
| **NFR-4** | Profile auto-population from ATProto SHALL add no more than 2 seconds to the login flow. |
| **NFR-5** | Avatar uploads SHALL be limited to 5MB before resizing. Resized output SHALL not exceed 100KB. |
| **NFR-6** | All vCard operations SHALL work on Cloudflare Workers (no Node.js-only APIs). |

## 5. Data Model

### Existing: `vcard_storage` table (server-side, already exists)

```sql
CREATE TABLE vcard_storage (
    jid TEXT PRIMARY KEY,
    vcard_xml TEXT NOT NULL,
    created_at DATETIME,
    updated_at DATETIME
);
```

No schema changes required on the server.

### New: Frontend vCard Cache (client-side, in-memory)

```typescript
interface VCardData {
  jid: string;
  fullName: string | null;
  photoUrl: string | null;      // EXTVAL URL or data: URI from BINVAL
  description: string | null;
  rawXml: string;               // preserved for round-trip editing
  fetchedAt: number;            // timestamp for cache freshness
}

// Stored in a Pinia store or composable, keyed by bare JID
Map<string, VCardData>
```

### New: Profile metadata flag (server-side)

To track whether a vCard was auto-populated or user-modified (for FR-1.3):

```sql
ALTER TABLE vcard_storage ADD COLUMN source TEXT DEFAULT 'manual';
-- Values: 'atproto_auto' | 'manual'
```

## 6. API Contracts

### XMPP IQ: Fetch vCard (XEP-0054)

**Request:**
```xml
<iq type="get" to="contact@waddle.social" id="vcard-1">
  <vCard xmlns="vcard-temp"/>
</iq>
```

**Response (success):**
```xml
<iq type="result" from="contact@waddle.social" id="vcard-1">
  <vCard xmlns="vcard-temp">
    <FN>David Flanagan</FN>
    <PHOTO>
      <EXTVAL>https://cdn.bsky.app/img/avatar/plain/did:plc:abc123/cid@jpeg</EXTVAL>
    </PHOTO>
    <DESC>Software engineer, open source enthusiast</DESC>
  </vCard>
</iq>
```

**Response (no vCard):**
```xml
<iq type="result" from="contact@waddle.social" id="vcard-1">
  <vCard xmlns="vcard-temp"/>
</iq>
```

### XMPP IQ: Update Own vCard (XEP-0054)

**Request:**
```xml
<iq type="set" id="vcard-set-1">
  <vCard xmlns="vcard-temp">
    <FN>New Display Name</FN>
    <PHOTO>
      <TYPE>image/png</TYPE>
      <BINVAL>base64encodedimagedata...</BINVAL>
    </PHOTO>
  </vCard>
</iq>
```

**Response:**
```xml
<iq type="result" id="vcard-set-1"/>
```

### Colony → XMPP Server: Auto-Populate vCard (New Internal API)

During ATProto OAuth callback, Colony calls the XMPP server to set the user's vCard. This could be:
- **Option A:** Colony sends an XMPP admin IQ on behalf of the user (requires admin API)
- **Option B:** Colony writes directly to the `vcard_storage` D1 table via shared database access
- **Option C:** Colony stores profile data in KV (already does this), and the XMPP server reads it on first connection

**Decision needed:** Which mechanism to use. Option C is simplest given Colony already caches profile data in KV (`user:${userDid}`).

## 7. Non-Goals

- **Full vCard editing** — We will NOT expose all vCard fields (ORG, TEL, ADR, etc.). Only display name and avatar.
- **XEP-0292 (vCard4)** — We will NOT implement the newer vCard4 over PubSub standard. XEP-0054 is sufficient and already implemented.
- **Avatar hash presence (XEP-0153)** — Server-side implementation of XEP-0153 is a stretch goal, not required for v1.
- **Profile visibility/privacy controls** — All vCards are readable by any authenticated user. No per-field privacy.
- **Animated avatar support** — GIFs/APNGs are not supported; static images only.
- **Profile banner images** — Not in scope.

## 8. Open Questions

| # | Question | Impact |
|---|----------|--------|
| OQ-1 | How should Colony communicate profile data to the XMPP server for auto-population? (See API Contracts §6, Options A/B/C) | Determines server-side implementation approach |
| OQ-2 | Should vCards be refreshed from ATProto periodically (e.g., daily) or only on login? | Affects staleness of display names/avatars |
| OQ-3 | Should the `source` column track `atproto_auto` vs `manual` to prevent overwriting user edits, or should a simpler "last-modified-by" flag be used? | Affects FR-1.3 implementation |
| OQ-4 | Does the XMPP server currently handle `vCard xmlns="vcard-temp"` IQ gets/sets in the stanza handler, or only in the storage layer? Need to verify the IQ routing exists. | Could require server-side work |

## 9. Risks & Dependencies

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| Bluesky CDN avatar URLs may expire or change format | Medium | Users see broken avatar images | Store as EXTVAL but implement fallback to generated avatar; consider binary storage later |
| XEP-0054 IQ routing may not be fully wired in server stanza handler | Medium | vCard fetch/set silently fails | Verify server handles `vcard-temp` namespace in IQ get/set before frontend work |
| Large roster = many vCard fetches = slow initial load | Medium | Poor UX on first connection | Throttle concurrent fetches (NFR-1); lazy-load avatars (NFR-3) |
| Colony and XMPP server share D1 database — concurrent writes | Low | Race condition on vCard writes | Use UPSERT (already implemented); document write ordering |

## 10. Decisions Made

| # | Decision | Rationale |
|---|----------|-----------|
| D-1 | Use XEP-0054 (vcard-temp), not XEP-0292 (vCard4) | Already implemented server-side; simpler; widely supported |
| D-2 | Store ATProto avatar as URL reference (EXTVAL), not binary | Simpler; avoids downloading/encoding large images; Bluesky CDN is reliable |
| D-3 | Profile editing limited to display name + avatar | Keep scope minimal; full vCard editing is unnecessary for a chat app |
| D-4 | Both ATProto and SCRAM users supported | ATProto auto-populates; SCRAM users set profile manually via editing UI |
| D-5 | Client-side avatar resizing before upload | Avoids server-side image processing; keeps vCard blobs small |

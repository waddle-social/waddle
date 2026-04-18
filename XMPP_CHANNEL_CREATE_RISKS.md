# Adversarial Design Review: Pure-XMPP Channel Creation

## Executive Summary

**Recommendation:** Extend XEP-0503 Spaces writes as the primary path, with XEP-0050 ad-hoc commands as a secondary option for power users. Direct MUC room creation should be explicitly blocked for managed channels.

**Key Risk:** Protocol fragmentation—allowing multiple creation paths will lead to inconsistent state, confused clients, and support nightmares.

---

## Option Analysis

### Option 1: Extend XEP-0503 Spaces Writes (RECOMMENDED)

**Current State:**
- Read-only Spaces implementation exists (`xep/xep0503.rs`)
- Advertises full feature set but returns `service-unavailable` for writes
- Already exposes channels as pubsub items with XEP-0402 bookmarks
- Integration with disco#items and pubsub queries

**Approach:**
Accept pubsub `<publish>` stanzas on the spaces node to create new channels. Parse channel metadata from the bookmark element, write to the waddle DB, create the MUC room, then reflect the new channel via pubsub notifications.

**Risks (HIGH TO CRITICAL):**

1. **Race Conditions in Multi-Client Create** (P0)
   - Concurrent publishes to the same waddle from different clients
   - No atomic DB+MUC creation; partial failure states exist
   - Example: Channel created in DB but MUC room fails → ghost channel
   - Mitigation: Implement idempotent create with distributed locks on waddle_id+channel_name

2. **Permission Bypass via Direct Pubsub** (P0)
   - Current HTTP API checks `create_channel` permission against waddle
   - Pubsub publish handler MUST duplicate this check or risk unauthorized creation
   - Easy to forget when porting HTTP logic to XMPP handlers
   - Mitigation: Centralize permission check in shared function called by both paths

3. **Bookmark Schema Mismatch** (P1)
   - XEP-0402 bookmarks are minimal (JID, name, autojoin)
   - Waddle channels have: description, position, channel_type (text/forum)
   - Extended fields require custom `<extensions>` child → client compatibility risk
   - Mitigation: Define strict Waddle-specific bookmark schema, document fallback behavior

4. **Pubsub Notification Fanout Failure** (P1)
   - After channel creation, all waddle members should receive pubsub notify
   - If notification system is down/slow, clients desync
   - No retry mechanism for failed pubsub broadcasts in current codebase
   - Mitigation: Make channel visible immediately via disco#items even if notifications fail

5. **Spaces Node vs MUC Domain Confusion** (P2)
   - Publish goes to `spaces.waddle.social` but room is on `muc.waddle.social`
   - Clients may try to join `spaces.waddle.social/room-name` instead of the MUC JID
   - Bookmark element MUST contain full MUC JID, not a relative reference
   - Mitigation: Validate JID format in bookmark parser, reject non-MUC JIDs

6. **No Atomic Rollback Path** (P1)
   - Create sequence: waddle DB insert → permission tuple → MUC room → pubsub notify
   - Any mid-sequence failure leaves inconsistent state
   - HTTP handler has partial cleanup (delete channel if permissions fail) but incomplete
   - Mitigation: Implement compensating transaction pattern with explicit rollback steps

**Architecture Strengths:**
- Natural fit: Spaces already represents waddles, channels are already items
- Clients already query Spaces for discovery, extending to writes is intuitive
- Pubsub notifications give live updates to all members automatically
- XEP-0503 is the "blessed" path for community structure in modern XMPP

---

### Option 2: XEP-0050 Ad-Hoc Commands

**Current State:**
- Helpers exist (`xep/xep0050.rs`) with parsers/builders
- Runtime does NOT advertise commands (test confirms `service-unavailable`)
- No command registry, no session state tracking for multi-step flows

**Approach:**
Advertise a `create-channel` command node. Client executes, server responds with a data form (name, description, type, position), client submits, server creates channel and returns result.

**Risks (MODERATE TO HIGH):**

1. **Session State Management Hell** (P0)
   - Ad-hoc commands are stateful multi-turn flows
   - Need to track incomplete command sessions per user+waddle
   - Session timeout, cancellation, concurrent sessions = complexity explosion
   - Current codebase has zero session state infra for commands
   - Mitigation: Use in-memory session cache with TTL, but this adds latency and failure modes

2. **Command Discovery UX Friction** (P1)
   - Users must disco#items on `localhost` with node=`http://jabber.org/protocol/commands`
   - Then filter for `create-channel` command
   - Then execute with waddle context (how? pass waddle_id as initial arg?)
   - More client round-trips than pubsub publish
   - Mitigation: Pre-populate command list in client UI, but still requires waddle_id injection

3. **No Standard Channel Context** (P1)
   - Command executes against server root, not against a specific waddle node
   - Client must pass waddle_id as a hidden form field
   - Easy to spoof if not validated (create channel in wrong waddle)
   - Mitigation: Parse `sessionid` or waddle_id from command args, re-check permissions

4. **Form Validation Is Manual** (P2)
   - XEP-0004 data forms have basic typing (text-single, boolean, list-single)
   - No built-in validation for "channel name must be unique in waddle"
   - Server must reject and re-prompt with error note
   - Another round-trip, poor UX compared to immediate failure
   - Mitigation: Pre-validate in client before submission (but client needs waddle state)

5. **No Live Updates** (P2)
   - Command execution is request/response only
   - After channel is created, other clients don't learn about it until they re-query Spaces
   - Must manually broadcast a pubsub notification anyway (so why not just use pubsub?)
   - Mitigation: Publish to Spaces node after command completes (hybrid approach adds complexity)

**Architecture Strengths:**
- Explicit, user-initiated action (good for power users, not casual users)
- Multi-step forms can guide users through complex config (useful for advanced channel types?)
- Server-driven validation with human-readable error messages

**Critical Flaw:**
You're building a stateful, session-based command system when you already have pubsub. This is over-engineering unless you need complex multi-step workflows (you don't for basic channel creation).

---

### Option 3: Direct MUC Room Creation (XEP-0045 §10.1.2)

**Current State:**
- Instant room creation exists (`room_registry.rs:create_instant_room`)
- User joins non-existent room → server creates it with default config
- Used for testing and ad-hoc rooms

**Approach:**
User joins `waddle-id_channel-name@muc.waddle.social`, server creates room and waddle DB entry on-the-fly.

**Risks (CATASTROPHIC):**

1. **No Waddle Association Enforcement** (P0)
   - User can create arbitrary rooms with any `waddle-id_` prefix
   - How do you know the user has permission for that waddle?
   - Instant rooms don't consult permission service
   - Mitigation: Reject all instant rooms with managed name patterns, but this breaks discoverability

2. **Name Collision Chaos** (P0)
   - Two users join `eng_general@muc.waddle.social` simultaneously
   - Both create the channel in different waddles (or same waddle twice)
   - Race condition in DB insert (unique constraint violation likely but not guaranteed)
   - Mitigation: Pre-allocate channel IDs in waddle DB, but this defeats "instant" rooms

3. **No Metadata Capture** (P0)
   - Instant rooms have no description, no position, no channel_type
   - Cannot distinguish text vs forum channels
   - Frontend sees bare MUC rooms, cannot render properly
   - Mitigation: Require MUC room config submission before allowing messages (breaks UX flow)

4. **Spaces Desync** (P0)
   - Channel created via MUC join doesn't appear in Spaces pubsub items
   - Spaces discovery queries miss it until manual sync
   - No hook to publish to Spaces node from MUC room creation path
   - Mitigation: Poll MUC room list and sync to Spaces on interval (horrible, eventual consistency issues)

5. **Permission Escalation** (P0)
   - Instant room creator gets owner affiliation
   - If they're not a waddle admin, they shouldn't have owner on managed channels
   - Existing instant room logic doesn't check waddle roles
   - Mitigation: Disable instant room creation entirely for managed room JID patterns

**Verdict: DO NOT ALLOW direct MUC creation for managed channels.**

Instant rooms are fine for ad-hoc testing/private chats, but mixing them with the managed Waddle channel system is a security and consistency disaster.

---

### Option 4: Hybrid Approaches (ANTI-PATTERN)

**What Not To Do:**

1. **"Support all three methods"**
   - Three code paths = three times the bugs
   - Clients don't know which to use
   - Support requests: "I created a channel via ad-hoc command but it doesn't show up in Spaces"
   - State synchronization becomes a full-time job

2. **"Use ad-hoc commands to pre-create, then pubsub to publish"**
   - Why? Just use pubsub for both
   - Adding command layer doesn't improve anything, only adds latency

3. **"Allow MUC instant rooms but sync them to Spaces afterward"**
   - Race-prone, permission bypass risk, eventual consistency hell
   - Better to fail fast with a clear error than silently create broken state

---

## Recommended Implementation Plan

**Primary Path: XEP-0503 Spaces Writes**

1. **Accept pubsub `<publish>` on `spaces.waddle.social` node=`{waddle_id}`**
   - Parse item ID as channel identifier (or generate UUID)
   - Extract channel metadata from bookmark `<extensions>` child
   - Validate: user has `create_channel` permission on waddle

2. **Atomic create with compensation:**
   ```rust
   // Pseudo-code
   let channel_id = uuid::new_v4();
   let tx = db.begin_transaction();
   tx.insert_channel(waddle_id, channel_id, metadata)?;
   tx.insert_permission_tuple(channel#parent@waddle)?;
   tx.commit()?;
   
   // If this fails, rollback DB transaction and return error
   let room = room_registry.create_room(muc_jid, waddle_id, channel_id, config)
       .map_err(|e| {
           tx.rollback();
           e
       })?;
   
   // Best-effort notify (failure is non-fatal, clients will resync on next disco query)
   let _ = pubsub.notify_subscribers(waddle_id, new_channel_item);
   ```

3. **Bookmark schema extension (Waddle namespace):**
   ```xml
   <item id='uuid-here'>
     <conference xmlns='urn:xmpp:bookmarks:1' 
                 name='General' 
                 autojoin='true'>
       <extensions xmlns='https://waddle.social/schemas/channel/1.0'>
         <description>Main discussion channel</description>
         <position>0</position>
         <type>text</type>
       </extensions>
     </conference>
   </item>
   ```

4. **Explicit rejection of instant managed rooms:**
   ```rust
   if room_jid.node().unwrap_or("").contains('_') {
       // Managed room pattern detected
       return Err(XmppError::not_allowed(
           "Managed channels must be created via Spaces pubsub, not direct MUC join"
       ));
   }
   ```

**Secondary Path: XEP-0050 Ad-Hoc Commands (for power users only)**

- Implement ONLY if there's a clear use case (e.g., scripted channel creation via CLI)
- Command MUST delegate to the same pubsub publish handler (no duplicate logic)
- Document that pubsub is the canonical method

---

## What To Explicitly Avoid

1. **Do NOT allow direct MUC instant room creation for managed channels**
   - Security risk, state consistency nightmare

2. **Do NOT implement ad-hoc commands as the primary path**
   - Over-engineered for simple channel creation

3. **Do NOT write separate permission checks in the XMPP handler**
   - Extract HTTP handler's permission logic to shared function, call from both

4. **Do NOT assume pubsub notifications always succeed**
   - Make disco#items the source of truth, notifications are optimization

5. **Do NOT skip transaction rollback on MUC room creation failure**
   - Ghost channels in DB are worse than failed creation

---

## Open Questions for User

1. **Channel naming:** Should channel IDs be user-chosen slugs or server-generated UUIDs?
   - Current HTTP API: UUID
   - Pubsub item ID: could be slug (more readable JIDs)
   - Recommendation: Allow both, but validate slug uniqueness strictly

2. **Forum channel special handling:** Do forum channels need different MUC config on creation?
   - Current code sets `muc#roomconfig_forum: true` in room config
   - Should pubsub publish handler auto-detect `<type>forum</type>` and set this?
   - Recommendation: Yes, map channel_type to MUC config in create handler

3. **Partial failure notification strategy:**
   - If pubsub notification fails but channel is created, should server retry?
   - Or rely on client-side polling/resync?
   - Recommendation: Log failure, increment metric, let clients resync (avoid infinite retry loops)

---

## Metrics & Monitoring

**Add these to detect issues in production:**

- `waddle_channel_create_attempts_total{method="spaces_pubsub"|"adhoc_command"|"instant_muc"}`
- `waddle_channel_create_failures_total{reason="permission_denied"|"db_error"|"muc_error"|"pubsub_error"}`
- `waddle_channel_create_rollbacks_total` (tracks compensation transaction invocations)
- `waddle_spaces_pubsub_notification_failures_total` (detect silent failures)

---

## Conclusion

**Use XEP-0503 Spaces pubsub writes.** It's the modern, notification-aware, natural extension of your existing architecture. Ad-hoc commands are overkill. Direct MUC creation is a landmine.

**Biggest risk you MUST address:** Atomic channel creation with proper rollback. The current HTTP handler's partial cleanup is not sufficient. A failed MUC room creation must undo the DB insert and permission tuple, or you'll leak ghost channels.

**Second biggest risk:** Permission checks. Do NOT duplicate logic. Extract to shared validation layer.

# Admin V2 implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans. Steps use `- [ ]` syntax.

**Goal:** Light up two of admin V1's stub panels with full read+write — Spaces (6 commands) and Channels (8 commands) — and make the entire admin surface mobile-friendly via the existing `ChatMobileDrawers` pattern.

**Architecture:** All admin operations flow over a single Waddle-namespaced ad-hoc command family (`urn:waddle:admin:*`). The server's admin command handlers delegate to existing internal APIs (`spaces_pubsub_seed`, MUC actor, pubsub admin storage) rather than re-implementing storage logic. The chat client mirrors V1's admin layout but extends it with mobile drawers.

**Tech stack:** Rust (server) + TypeScript/Astro/Vue 3/Tailwind 4 (chat) + Bun. Reuses everything V1 already built.

**Spec:** `docs/superpowers/specs/2026-05-17-admin-v2-design.md` (this branch, first commit).

**PR:** TBD at `feat/server-admin-v2-spaces-channels` off main.

---

## Pre-work for the agent

Before starting Task 1, read these end-to-end to internalize V1 patterns this plan mirrors:

1. `server/crates/waddle-server/src/admin/mod.rs` — `is_community_owner` ACL helper. Every admin command checks this.
2. `server/crates/waddle-server/src/admin/users_list.rs` — V1 ad-hoc command handler. The 14 V2 commands all follow this exact shape: parse args data form, ACL-check, delegate, return result data form. Mirror it.
3. `server/crates/waddle-xmpp/src/admin.rs` — V1 namespace constant declarations. New constants go alongside; one per command.
4. `server/crates/waddle-server/tests/admin_users_list_command_ws.rs` — V1 integration tests. Same harness powers V2.
5. `server/crates/waddle-xmpp-client-wasm/src/client_admin.rs` — V1 wasm boundary. New methods slot in alongside `admin_users_list`.
6. `chat/src/components/admin/AdminLayout.vue` + `AdminUsersPanel.vue` — V1 chat components. Spaces / Channels panels mirror the Users-panel shape.
7. `chat/src/components/chat/UserProfileDrawer.vue` — slide-from-right pattern V2 detail drawers reuse.
8. `chat/src/components/chat/ChatMobileDrawers.vue` — mobile drawer pattern.

CLAUDE.md hard rules apply to every commit:
- `cargo fmt` before every commit
- No `unwrap` in new Rust code
- No clippy allows; fix the code
- Typed payloads everywhere — no `String`-blob protocol data
- XML via `minidom::Element` builders, never `format!`
- Bun only in `chat/`, knip clean
- Conventional Commits with single scope per commit

Also: the existing `xmpp_e2e_cue.rs` test asserts every advertised feature is either mapped to a XEP or listed in `ADVERTISED_FEATURE_EXEMPTIONS`. The V1 fix added `urn:waddle:admin:users:list:0` to the exemption list. **Every new V2 namespace must be added there too**, or the test fails (we hit this on V1).

---

## Task 1: Namespace constants for the 14 new commands

**Files:**
- Modify: `server/crates/waddle-xmpp/src/admin.rs` (extend the existing constants)

- [ ] **Step 1: Add the new constants**

Open the file. After the existing `NS_ADMIN_USERS_LIST` constant, add:

```rust
// Spaces (XEP-0050 ad-hoc command nodes, Waddle-owned namespace)
pub const NS_ADMIN_SPACES_LIST: &str = "urn:waddle:admin:spaces:list:0";
pub const NS_ADMIN_SPACES_CREATE: &str = "urn:waddle:admin:spaces:create:0";
pub const NS_ADMIN_SPACES_UPDATE: &str = "urn:waddle:admin:spaces:update:0";
pub const NS_ADMIN_SPACES_DELETE: &str = "urn:waddle:admin:spaces:delete:0";
pub const NS_ADMIN_SPACES_MEMBERS: &str = "urn:waddle:admin:spaces:members:0";
pub const NS_ADMIN_SPACES_SET_ROLE: &str = "urn:waddle:admin:spaces:set-role:0";

// Channels
pub const NS_ADMIN_CHANNELS_LIST: &str = "urn:waddle:admin:channels:list:0";
pub const NS_ADMIN_CHANNELS_CREATE: &str = "urn:waddle:admin:channels:create:0";
pub const NS_ADMIN_CHANNELS_UPDATE: &str = "urn:waddle:admin:channels:update:0";
pub const NS_ADMIN_CHANNELS_DELETE: &str = "urn:waddle:admin:channels:delete:0";
pub const NS_ADMIN_CHANNELS_OCCUPANTS: &str = "urn:waddle:admin:channels:occupants:0";
pub const NS_ADMIN_CHANNELS_AFFILIATIONS: &str = "urn:waddle:admin:channels:affiliations:0";
pub const NS_ADMIN_CHANNELS_SET_AFFILIATION: &str = "urn:waddle:admin:channels:set-affiliation:0";
pub const NS_ADMIN_CHANNELS_KICK: &str = "urn:waddle:admin:channels:kick:0";
```

- [ ] **Step 2: Verify compiles + commit**

```bash
cd server && cargo check -p waddle-xmpp && cargo fmt && cd ..
git add server/crates/waddle-xmpp/src/admin.rs
git commit -m "feat(server): namespace constants for admin V2 spaces + channels commands

This commit was created with the assistance of a LLM."
```

---

## Task 2: Add namespaces to disco features + exemptions

**Files:**
- Modify: `server/crates/waddle-xmpp/src/disco/info.rs` (find where `NS_ADMIN_USERS_LIST` is pushed, push all 14 new constants alongside)
- Modify: `server/crates/waddle-server/tests/xmpp_e2e_cue.rs` `ADVERTISED_FEATURE_EXEMPTIONS` (add all 14 namespaces)

- [ ] **Step 1: Read the disco feature registration**

```bash
grep -n 'NS_ADMIN_USERS_LIST' server/crates/waddle-xmpp/src/disco/info.rs
```

Add each new constant in the same way the V1 one is pushed.

- [ ] **Step 2: Extend exemption list**

In `tests/xmpp_e2e_cue.rs`, find `ADVERTISED_FEATURE_EXEMPTIONS` and add all 14 new namespaces alphabetically alongside `"urn:waddle:admin:users:list:0"`.

- [ ] **Step 3: Verify + commit**

```bash
cd server && cargo fmt && cargo test -p waddle-server --test xmpp_e2e_cue advertised_features_have_cue_xep_coverage && cd ..
git add -u
git commit -m "feat(server): advertise admin V2 namespaces in disco + exemption list

This commit was created with the assistance of a LLM."
```

---

## Task 3: Spaces commands — list + create + update + delete

**Files:**
- Create: `server/crates/waddle-server/src/admin/spaces.rs`
- Modify: `server/crates/waddle-server/src/admin/mod.rs` (`pub mod spaces;`)
- Modify: wherever V1's `users_list` is registered with the command dispatcher (find via `grep -rn 'users_list::' server/crates/waddle-server/src/`)

This is the largest task. Follow the V1 `users_list.rs` shape exactly.

- [ ] **Step 1: Skeleton**

Create the file with:
- Module-level doc comment naming all six commands.
- Imports mirroring `users_list.rs`.
- Public typed args/result structs per command (one Rust struct per arg/result data form).

For example, the typed shapes for the `list` command:

```rust
#[derive(Debug, Clone, Default)]
pub struct SpacesListArgs {
    pub prefix: Option<String>,
    pub page_size: Option<u32>,
    pub after_cursor: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SpaceListEntry {
    pub space_jid: BareJid,
    pub name: String,
    pub description: Option<String>,
    pub icon_url: Option<String>,
    pub channel_count: u32,
    pub member_count: u32,
}

#[derive(Debug, Clone, Default)]
pub struct SpacesListResult {
    pub entries: Vec<SpaceListEntry>,
    pub next_cursor: Option<String>,
}
```

Mirror the typed args/result style for `create`, `update`, `delete`.

- [ ] **Step 2: Data form parsers/builders**

For each of the four commands, write `parse_<command>_args` and `build_<command>_result` functions. These mirror exactly what `users_list.rs` does: `<x type='submit'>` → struct; struct → `<x type='result'>`. Use `xmpp_parsers::data_forms`.

Reject invalid args with a typed `AdminError`. No `unwrap`, no `String` error bodies that aren't going to the `Log` outbound event.

- [ ] **Step 3: Handlers**

For each command, write `handle_<command>(state, sender, args) -> Result<Result, AdminError>` that:

1. ACL-checks via `is_community_owner(state, sender)`.
2. Validates args (name length 1–80, etc.).
3. Delegates:
   - `list` → query the spaces table / pubsub-spaces storage (find via `grep -rn 'spaces_pubsub_seed\|SpaceStore' server/crates/waddle-server/src/`). For channel_count/member_count, count from the existing tables.
   - `create` → call into the spaces seed code that already exists for first-run setup. If no reusable helper exists, follow the same DB + pubsub flow that the seed uses; extract a helper if it reduces duplication.
   - `update` → mutate the space record + republish PEP if Waddle uses PEP for space config (check `spaces_pubsub_seed.rs` to confirm).
   - `delete` → cascade-destroy. Iterate child channels and call MUC actor `destroy_room`; then delete the space record. Wrap in a single transaction if possible.

- [ ] **Step 4: Register with the command dispatcher**

Find the V1 registration site (`grep -rn 'NS_ADMIN_USERS_LIST' server/crates/waddle-server/src/`). Each new command registers identically.

- [ ] **Step 5: Compile + commit**

```bash
cd server && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cd ..
git add server/crates/waddle-server/src/admin/spaces.rs server/crates/waddle-server/src/admin/mod.rs server/crates/waddle-server/src/lib.rs
# plus wherever the dispatcher registration lives
git add -u
git commit -m "feat(server): admin spaces — list + create + update + delete (urn:waddle:admin:spaces:*)

This commit was created with the assistance of a LLM."
```

---

## Task 4: Spaces commands — members + set-role

**Files:**
- Modify: `server/crates/waddle-server/src/admin/spaces.rs` (extend with two more commands)

Same pattern as Task 3. Two more typed-args + parser + handler triples, two more dispatcher registrations.

- `members` paginates over the space's membership table (or whatever the equivalent is — check `spaces_pubsub_seed`).
- `set-role` changes a member's role and propagates the change to whatever stores it (likely both the space record and any XEP-0317 hat publication).

- [ ] **Step 1: Implement the two commands** following the pattern from Task 3.

- [ ] **Step 2: Commit**

```bash
cd server && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cd ..
git add -u
git commit -m "feat(server): admin spaces — members + set-role

This commit was created with the assistance of a LLM."
```

---

## Task 5: Spaces integration test suite

**Files:**
- Create: `server/crates/waddle-server/tests/admin_spaces_ws.rs`

Mirror `tests/admin_users_list_command_ws.rs`. One #[tokio::test] per case below. The fixture pattern (`common::*` helpers) is the same one V1 uses.

Required cases:

```
spaces_list_returns_empty_for_fresh_server
spaces_list_returns_seeded_spaces_with_counts
spaces_list_paginates_via_cursor
spaces_list_prefix_filter_narrows
spaces_list_forbidden_for_non_owner
spaces_create_persists_new_space
spaces_create_rejects_invalid_name
spaces_create_forbidden_for_non_owner
spaces_update_changes_name_description_icon
spaces_update_forbidden_for_non_owner
spaces_update_rejects_unknown_space
spaces_delete_cascades_to_channels
spaces_delete_requires_confirm_yes
spaces_delete_forbidden_for_non_owner
spaces_members_lists_with_role
spaces_members_forbidden_for_non_owner
spaces_set_role_promotes_to_admin
spaces_set_role_demotes_to_member
spaces_set_role_removes_with_none
spaces_set_role_forbidden_for_non_owner
```

20 cases. Use the existing `common::seed_*` helpers; do not invent new fixtures.

- [ ] **Step 1: Write the test file** following the V1 admin test pattern.

- [ ] **Step 2: Run**

```bash
cd server && cargo test -p waddle-server --test admin_spaces_ws && cd ..
```

Expected: 20 pass.

- [ ] **Step 3: Commit**

```bash
cd server && cargo fmt && cd ..
git add server/crates/waddle-server/tests/admin_spaces_ws.rs
git commit -m "test(server): admin_spaces_ws — 20-case integration matrix

This commit was created with the assistance of a LLM."
```

---

## Task 6: Channels commands — list + create + update + delete

**Files:**
- Create: `server/crates/waddle-server/src/admin/channels.rs`
- Modify: `server/crates/waddle-server/src/admin/mod.rs` (`pub mod channels;`)
- Modify: command dispatcher registrations

Same pattern as Task 3, but for channels. Key differences:

- `list` joins channels to their space + counts occupants + counts affiliations per tier. The occupant count is the live MUC actor's count (current presence); affiliation counts come from the MUC affiliation store.
- `create` calls the existing MUC create flow with default `muc_public=true`, `muc_persistent=true`, member-only=false (per user's "everything public by default" decision). If a `space_jid` is supplied, also link in the space's channel list.
- `update` calls MUC room-config IQ internally as room-owner (server bypass — admin doesn't need to be joined). Re-publishes the space's channel-list if the channel's space association changed (V2 leaves moves out of scope, but defensive code).
- `delete` calls MUC destroy.

Note that some of this logic exists in `server/crates/waddle-server/src/server/routes/websocket/handlers/iq/muc/*` — read those first to find the right internal entry points. **Do not re-implement MUC owner protocol from scratch.**

- [ ] **Step 1: Read existing MUC entry points**

```bash
ls server/crates/waddle-server/src/muc/ 2>/dev/null
grep -rn 'create_room\|destroy_room\|set_room_config' server/crates/waddle-server/src/muc/ | head -10
```

Find the actor / store methods. Document them in code comments.

- [ ] **Step 2: Implement the four commands**

Same shape as spaces. Each: typed args, parser/builder, handler, dispatcher registration.

- [ ] **Step 3: Compile + commit**

```bash
cd server && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cd ..
git add -u
git commit -m "feat(server): admin channels — list + create + update + delete

Defaults: public, persistent, not members-only. New channels are public
by default per spec.

This commit was created with the assistance of a LLM."
```

---

## Task 7: Channels commands — occupants + affiliations + set-affiliation + kick

**Files:**
- Modify: `server/crates/waddle-server/src/admin/channels.rs` (four more commands)

Same pattern. Key delegations:

- `occupants` reads from the MUC actor's live presence registry (the same source the existing room-occupant disco query reads). Includes role + affiliation per occupant.
- `affiliations` reads from the MUC affiliation store. Filterable by tier.
- `set-affiliation` calls MUC admin protocol's internal entry — XEP-0045 §10. Admin doesn't need to be joined. `outcast` is the ban.
- `kick` is a role-change to `none` via the MUC actor — XEP-0045 §9.1. Occupant gets a presence with `<status code='307'/>`.

- [ ] **Step 1: Implement**

Mirror the MUC owner/admin IQ handler bodies that exist for the regular wire path; just bypass the "must be joined" gate.

- [ ] **Step 2: Compile + commit**

```bash
cd server && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cd ..
git add -u
git commit -m "feat(server): admin channels — occupants + affiliations + set-affiliation + kick

This commit was created with the assistance of a LLM."
```

---

## Task 8: Channels integration test suite

**Files:**
- Create: `server/crates/waddle-server/tests/admin_channels_ws.rs`

Required cases:

```
channels_list_returns_empty_for_fresh_server
channels_list_includes_space_filter
channels_list_includes_occupant_and_affiliation_counts
channels_list_paginates_via_cursor
channels_list_forbidden_for_non_owner
channels_create_defaults_to_public_persistent
channels_create_with_space_link
channels_create_rejects_invalid_name
channels_create_forbidden_for_non_owner
channels_update_changes_name_topic_visibility
channels_update_forbidden_for_non_owner
channels_delete_destroys_room
channels_delete_requires_confirm_yes
channels_delete_forbidden_for_non_owner
channels_occupants_lists_with_role_and_affiliation
channels_occupants_forbidden_for_non_owner
channels_affiliations_lists_all_tiers
channels_affiliations_filter_narrows
channels_affiliations_forbidden_for_non_owner
channels_set_affiliation_promotes_to_admin
channels_set_affiliation_bans_to_outcast
channels_set_affiliation_clears_with_none
channels_set_affiliation_forbidden_for_non_owner
channels_kick_role_changes_to_none
channels_kick_forbidden_for_non_owner
```

25 cases.

- [ ] **Step 1: Write the test file**.
- [ ] **Step 2: Run + commit**

```bash
cd server && cargo test -p waddle-server --test admin_channels_ws && cargo fmt && cd ..
git add server/crates/waddle-server/tests/admin_channels_ws.rs
git commit -m "test(server): admin_channels_ws — 25-case integration matrix

This commit was created with the assistance of a LLM."
```

---

## Task 9: Wasm bindings for all 14 commands

**Files:**
- Modify: `server/crates/waddle-xmpp-client-wasm/src/client_admin.rs`
- Modify: `server/crates/waddle-xmpp-client-wasm/src/types.rs` (add typed Js-facing structs per command)

Mirror the V1 `admin_users_list` method. One #[wasm_bindgen] method per command. The Js-facing types match the TS interfaces (see Task 10).

- [ ] **Step 1: Add Js-facing types in types.rs**

For each command: a `WaddleAdmin<Cmd>Args` and `WaddleAdmin<Cmd>Result` Rust struct with `serde::Serialize`/`Deserialize`. Mirror the V1 `WaddleAdminUsersPage` shape.

- [ ] **Step 2: Add wasm methods**

For each command, follow the V1 pattern:

```rust
#[wasm_bindgen]
pub fn admin_spaces_list(&self, args: JsValue) -> Promise {
    let inner = self.inner.clone();
    future_to_promise(async move {
        let parsed: WaddleAdminSpacesListArgs = serde_wasm_bindgen::from_value(args)?;
        let iq = build_admin_spaces_list_iq(&parsed)?;
        let response = inner.send_iq(iq).await.map_err(to_js_err)?;
        let result = parse_admin_spaces_list_response(&response)
            .ok_or_else(|| JsValue::from_str("malformed response"))?;
        to_js_value(&result)
    })
}
```

(The IQ builders and response parsers live in a new `server/crates/waddle-xmpp-client/src/xep/admin_v2.rs` — Rust client crate side, mirroring how V1 has admin code split across the wasm and base crates.)

- [ ] **Step 3: Regenerate wasm-pkg + commit**

```bash
cd chat && bun run wasm:build && cd ..
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings
git add -u
git add server/wasm-pkg/
git commit -m "feat(server): wasm bindings for admin V2 spaces + channels commands

This commit was created with the assistance of a LLM."
```

---

## Task 10: TypeScript types + BrowserXmppClient wrappers

**Files:**
- Modify: `chat/src/lib/xmpp/wasm-types.ts` (add 14 interface pairs)
- Modify: `chat/src/lib/xmpp/client.ts` (add 14 method wrappers on `BrowserXmppClient`)

Mirror V1's `WasmAdminUsersPage` + `adminUsersList`. One TS interface for each Args + Result pair, one method wrapper per command.

- [ ] **Step 1: TS interfaces** matching the Rust types from Task 9.
- [ ] **Step 2: BrowserXmppClient wrappers** — each calls `xmpp.admin_<cmd>?.(args)` and returns the typed result or null on transport error (mirror V1 `adminUsersList`).
- [ ] **Step 3: Knip + commit**

```bash
cd chat && bun run lint && cd ..
git add -u
git commit -m "feat(chat): wasm-types + BrowserXmppClient wrappers for admin V2 commands

This commit was created with the assistance of a LLM."
```

---

## Task 11: SpacesPanel + SpaceDetailDrawer + SpaceCreateDialog

**Files:**
- Create: `chat/src/components/admin/SpacesPanel.vue`
- Create: `chat/src/components/admin/SpaceDetailDrawer.vue`
- Create: `chat/src/components/admin/SpaceCreateDialog.vue`

Pattern: mirror `AdminUsersPanel.vue` for the list + search. Mirror `UserProfileDrawer.vue` for the detail drawer (slide-from-right on mobile, anchored right-pane on `lg`+).

- [ ] **Step 1: SpacesPanel.vue**

List of spaces with prefix search input and a "+" button (top-right) that opens `SpaceCreateDialog`. Each row shows name + channel-count + member-count chip. Click opens the detail drawer.

```vue
<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import type { WasmAdminSpacesListResult, WasmAdminSpaceListEntry } from "@/lib/xmpp/wasm-types";
import SpaceDetailDrawer from "@/components/admin/SpaceDetailDrawer.vue";
import SpaceCreateDialog from "@/components/admin/SpaceCreateDialog.vue";

const props = defineProps<{ xmppClient: BrowserXmppClient | null }>();

const entries = ref<WasmAdminSpaceListEntry[]>([]);
const loading = ref(true);
const error = ref<string | null>(null);
const selected = ref<WasmAdminSpaceListEntry | null>(null);
const showCreate = ref(false);
const prefix = ref("");

async function refresh() {
  if (!props.xmppClient) return;
  loading.value = true;
  try {
    const page = await props.xmppClient.adminSpacesList({
      prefix: prefix.value || undefined,
      page_size: 50,
    });
    entries.value = page?.entries ?? [];
  } catch (e) {
    error.value = String(e);
  } finally {
    loading.value = false;
  }
}

onMounted(refresh);
</script>
```

(Continue with template: search input, +-button, list rendering, drawer mount.)

- [ ] **Step 2: SpaceDetailDrawer.vue**

Loaded with the selected space. Has tabs/sections:
- **Editor:** name + description fields, Save button. Calls `adminSpacesUpdate`.
- **Members:** list with role chip per member; chip click → role dropdown → calls `adminSpacesSetRole`.
- **Danger:** Delete button → ConfirmDialog ("This will delete the space and all N channels under it") → calls `adminSpacesDelete` with `confirm: "yes"`.

Slide-in pattern from `UserProfileDrawer.vue`.

- [ ] **Step 3: SpaceCreateDialog.vue**

Modal with name + description fields. Submit calls `adminSpacesCreate`, on success closes and refreshes panel.

- [ ] **Step 4: Mount in /admin/spaces**

Update `chat/src/pages/admin/spaces.astro` (probably already a stub from V1; replace its content with the actual SpacesPanel mount). If not present, mirror `chat/src/pages/admin/users.astro`.

- [ ] **Step 5: Commit**

```bash
cd chat && bun test SpacesPanel && bun run lint && cd ..
git add -u
git commit -m "feat(chat): SpacesPanel + SpaceDetailDrawer + SpaceCreateDialog

Mobile-friendly: detail drawer slides from right on small screens,
anchors as right-pane on lg+. Reuses UserProfileDrawer slide pattern.

This commit was created with the assistance of a LLM."
```

---

## Task 12: ChannelsPanel + ChannelDetailDrawer + ChannelCreateDialog

**Files:**
- Create: `chat/src/components/admin/ChannelsPanel.vue`
- Create: `chat/src/components/admin/ChannelDetailDrawer.vue`
- Create: `chat/src/components/admin/ChannelCreateDialog.vue`

Same pattern as Task 11. Differences:

- **Panel** has a space-filter chip row above the search input.
- **DetailDrawer** has three tabs: Config (name/topic/is_public), Affiliations (list + per-row dropdown to change tier), Occupants (read-only list with kick button per row). Plus Delete in a Danger section.
- **CreateDialog** has name, topic, space (optional dropdown), is_public toggle (default true).

- [ ] **Step 1: ChannelsPanel.vue** — mirror SpacesPanel.
- [ ] **Step 2: ChannelDetailDrawer.vue** — three-tab layout. Tabs via Ark-UI Tabs primitive (search the codebase for existing usage).
- [ ] **Step 3: ChannelCreateDialog.vue** — `is_public` defaults to `true`. UI control should make this clear (toggle labeled "Public" + helper text "Anyone in the community can join").
- [ ] **Step 4: Mount in /admin/channels**.
- [ ] **Step 5: Commit**

```bash
cd chat && bun test ChannelsPanel && bun run lint && cd ..
git add -u
git commit -m "feat(chat): ChannelsPanel + ChannelDetailDrawer + ChannelCreateDialog

New channels default to public. Three-tab detail drawer: Config /
Affiliations / Occupants. Mobile pattern matches SpacesPanel.

This commit was created with the assistance of a LLM."
```

---

## Task 13: AdminLayout mobile drawer + responsive grid

**Files:**
- Modify: `chat/src/components/admin/AdminLayout.vue`

V1 has a fixed left sidebar. V2 makes it a drawer on `<lg` and a pinned column on `lg+`.

- [ ] **Step 1: Add drawer state**

```vue
<script setup lang="ts">
import { ref } from "vue";
// ... existing imports

const sidebarOpen = ref(false);
</script>
```

- [ ] **Step 2: Layout — responsive grid**

```vue
<template>
  <div class="grid h-full w-full grid-cols-1 lg:grid-cols-[16rem_1fr]">
    <!-- Mobile hamburger header -->
    <header class="flex items-center gap-2 p-3 lg:hidden">
      <button type="button" @click="sidebarOpen = true" aria-label="Open admin nav">
        <Menu class="h-5 w-5" />
      </button>
      <span class="type-pane-title">Admin</span>
    </header>

    <!-- Sidebar: pinned on lg+, slide-from-left on <lg -->
    <aside
      class="lg:block"
      :class="sidebarOpen ? 'fixed inset-0 z-50 bg-background lg:static lg:bg-transparent' : 'hidden'"
    >
      <!-- existing sidebar markup -->
    </aside>

    <!-- Main panel mount point -->
    <main class="overflow-y-auto">
      <slot />
    </main>
  </div>
</template>
```

(Adjust to whatever exact slot structure V1 uses — the agent inspects the V1 file first.)

- [ ] **Step 3: Backdrop + close-on-route-change**

On mobile, tapping the backdrop or selecting a new admin panel should close the drawer (`sidebarOpen.value = false` in the panel-select handler).

- [ ] **Step 4: Commit**

```bash
cd chat && bun run lint && cd ..
git add -u
git commit -m "feat(chat): AdminLayout mobile drawer + responsive grid

Sidebar collapses to a slide-from-left drawer below the lg breakpoint;
pinned 16rem column on lg+. Matches the rest of the chat's mobile UX.

This commit was created with the assistance of a LLM."
```

---

## Task 14: Vitest for spaces + channels panels

**Files:**
- Create: `chat/tests/admin-spaces-panel.test.ts`
- Create: `chat/tests/admin-channels-panel.test.ts`

Cases per panel:
- Renders the list on mount
- Search input filters results (calls `admin*List` with prefix)
- Click row opens detail drawer
- Create dialog submits correct args
- Delete confirm dialog requires explicit "yes"
- Mobile breakpoint (`window.innerWidth = 600`) shows drawer instead of pane

Mock `BrowserXmppClient` with stub methods returning canned `WasmAdminSpacesListResult` etc.

- [ ] **Step 1: Write both test files**.
- [ ] **Step 2: Run + commit**

```bash
cd chat && bun test admin-spaces-panel admin-channels-panel && bun run lint && cd ..
git add chat/tests/
git commit -m "test(chat): admin spaces + channels panels happy paths

This commit was created with the assistance of a LLM."
```

---

## Task 15: Conformance audit + final verification

**Files:**
- Modify: `docs/xep-conformance-audit.md`

- [ ] **Step 1: Add admin V2 rows under the "Waddle-namespaced extensions" section** the V1 audit-doc commit added. One row per of the 14 namespaces.

- [ ] **Step 2: Run the full local battery**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
cd chat && bun test && bun run lint && cd ..
```

All green expected.

- [ ] **Step 3: Push everything to remote**

```bash
git push -u origin feat/server-admin-v2-spaces-channels
```

- [ ] **Step 4: Open PR**

```bash
gh pr create --draft --base main --head feat/server-admin-v2-spaces-channels \
  --title "feat: admin V2 — spaces + channels CRUD, mobile-friendly" \
  --body-file <(cat <<'EOF'
## Summary

Lights up two of admin V1's stub panels with full CRUD over a single Waddle-namespaced ad-hoc command family:

- **Spaces** — list, create, update, delete; members + role management
- **Channels** — list, create, update, delete; occupants, affiliations, set-affiliation (owner/admin/member/outcast), kick

Mobile-friendly throughout via the existing `ChatMobileDrawers` pattern.

New channels default to **public** (`muc_public=true`, not members-only).

Full design: `docs/superpowers/specs/2026-05-17-admin-v2-design.md`.

## Wire surface

14 new ad-hoc commands under `urn:waddle:admin:*`. All ACL-gated on community-owner. Disco-advertised, exemption-listed.

## Test plan

- [ ] `cargo test --workspace` green (20 + 25 new integration cases)
- [ ] `cargo clippy -D warnings` clean
- [ ] `bun test && bun run lint` clean
- [ ] Manual mobile breakpoint check at 375px

This PR was created with the assistance of a LLM.
EOF
)
```

- [ ] **Step 5: Final commit**

```bash
git add docs/xep-conformance-audit.md
git commit -m "docs(server): conformance audit — admin V2 namespaces

This commit was created with the assistance of a LLM."
git push
```

---

## Self-review notes

- Every spec requirement (Goals 1–5, the 14 commands, mobile pattern, public-by-default) maps to at least one task: 1–3 → Spaces, 6–7 → Channels, 11–12 → UI, 13 → Mobile, 4–5/8 → Tests, 15 → Audit doc.
- Type-name consistency: server `Spaces*` / `Channels*` ↔ wasm `WaddleAdmin<X>` ↔ TS `WasmAdmin<X>`. Each layer's naming is internally consistent; conversions explicit.
- Public-by-default: enforced in `channels_create` handler default + reflected in `ChannelCreateDialog` UI default + asserted in `channels_create_defaults_to_public_persistent` test.
- Cascade delete: `spaces_delete_cascades_to_channels` test pins the contract.
- ACL: every command has a `_forbidden_for_non_owner` test.

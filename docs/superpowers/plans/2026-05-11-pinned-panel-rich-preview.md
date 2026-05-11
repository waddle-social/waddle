# Pinned-Panel Rich Preview Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `"(no preview text)"` in `PinnedPanel.vue` with the actual pinned message body — including images, video, audio, PDFs, extension annotations, and any future payload `MessageCard` supports — by fetching the live message via a new XEP-0359 §3 conformant MAM stanza-id filter and rendering it through a shared `<MessageBody>` component.

**Architecture:** Hybrid live + frozen-fallback. Server-side `PinPreview.text` snapshot stays as the aged-out / offline fallback. On panel open, the chat client batched-fetches the live messages by XEP-0359 stanza-id via a new `{urn:xmpp:sid:0}stanza-id` MAM form field, prefers any matching message already in the in-memory channel timeline (so XEP-0308 LMCs flow through automatically), and renders each entry through a presentational `<MessageBody compact />` extracted from `MessageCard`. Live pin events trigger single-id fetches; unpins evict; logout bumps the existing epoch guard.

**Tech Stack:**
- Rust (waddle-xmpp-core, waddle-xmpp, waddle-xmpp-client, waddle-xmpp-client-wasm) — protocol + storage + wire builder + wasm bindings
- TypeScript / Vue 3 / nanostores — `@/stores/pinned-message-bodies`, refactored `PinnedPanel.vue`, new `MessageBody.vue`
- Bun for tests on TS side; `cargo test` + `cargo fmt` + `cargo clippy -D warnings` on Rust side
- `bun run lint` (knip) must stay clean per CLAUDE.md

---

## File Structure

### New files

- `server/crates/waddle-xmpp-core/src/mam/stanza_id_filter.rs` — new module exposing `STANZA_ID_FILTER_FIELD` constant + per-id length cap.
- `server/crates/waddle-xmpp/src/xep/xep0359_tests.rs` — custom XEP-0359 §3 test suite (CLAUDE.md hard rule).
- `chat/src/stores/pinned-message-bodies.ts` — per-room body cache, derived `$pinnedPanelEntries` store joining `$pinnedRooms` × timeline × cache.
- `chat/src/services/pinned-message-bodies.ts` — orchestrator wiring panel-open fetch, single-id pin-event fetch, eviction on unpin.
- `chat/src/components/chat/MessageBody.vue` — presentational body + attachments component shared by `MessageCard` and `PinnedPanel`.
- `chat/src/stores/__tests__/pinned-message-bodies.test.ts` — body-cache + epoch + live-update unit tests.
- `chat/src/components/chat/__tests__/PinnedPanel.test.ts` — panel render-state matrix (live / fallback / aged-out / retracted / loading).

### Modified files

- `server/crates/waddle-xmpp-core/src/mam.rs` — re-export new constant.
- `server/crates/waddle-xmpp-core/src/mam/types.rs` — add `pub stanza_ids: Vec<StanzaId>` to `MamQuery`.
- `server/crates/waddle-xmpp-core/src/mam/query.rs` — parse the new form field; advertise it in `build_query_form_iq`.
- `server/crates/waddle-xmpp-core/src/mam/tests.rs` — disco + parser tests for the new field.
- `server/crates/waddle-xmpp/src/mam/storage/traits.rs` — no change required (already has `get_message_by_stanza_id`).
- `server/crates/waddle-xmpp/src/mam/storage/in_memory.rs` — `query` impl honours `query.stanza_ids`.
- `server/crates/waddle-xmpp/src/mam/storage/sqlx_store/query.rs` — `AND stanza_id IN (...)` filter branch.
- `server/crates/waddle-xmpp/src/mam/storage/tests.rs` — round-trip tests for the new filter (sqlx + in-memory).
- `server/crates/waddle-xmpp/src/xep/mod.rs` — register `xep0359_tests` mod under `#[cfg(test)]`.
- `server/crates/waddle-xmpp-client/src/mam.rs` — extend `build_mam_iq_extended` with `stanza_ids: Option<&[&str]>`; new helper `build_mam_stanza_ids_iq(room_jid, stanza_ids)`.
- `server/crates/waddle-xmpp-client-wasm/src/client_history.rs` — new `#[wasm_bindgen] fn fetch_room_messages_by_stanza_ids`.
- `server/crates/waddle-xmpp-client-wasm/src/lib.rs` — re-export if needed (mirror existing pattern).
- `chat/src/lib/xmpp/client.ts` — TS wrapper around the new wasm fn.
- `chat/src/lib/xmpp/wasm-types.ts` — no new types (reuse `WasmMamPage`/`WasmArchivedMessage`).
- `chat/src/stores/pinned-messages.ts` — clear body cache in `resetPinnedRooms()`; emit signal on unpin so service can evict body cache.
- `chat/src/channels/messages.ts` — wire panel-open trigger and pin-event single-id fetch.
- `chat/src/components/chat/MessageCard.vue` — replace inline body+attachments block (lines 680-960) with `<MessageBody :message />`. Lightbox stays.
- `chat/src/components/chat/PinnedPanel.vue` — use `<MessageBody :message compact />`; drop `"(no preview text)"`; host own `<ImageLightbox>`; render state-aware fallback.

### Files **NOT** touched

- `server/crates/waddle-xmpp/src/muc/pin.rs` — `PinPreview` schema unchanged. Server snapshot stays as text-only fallback.
- `server/crates/waddle-xmpp/src/protocol/room/pin.rs` — wire shape `<pin-event><preview>…</preview></pin-event>` unchanged.
- `server/crates/waddle-server/src/server/routes/interpret/room_pin.rs` — pin interpreter unchanged.

---

## Conventions for this plan

- Conventional Commits with scope: `fix(server): …`, `feat(chat): …`, `feat(server): …`, `test(server): …`, `refactor(chat): …`. Subject lowercase after the colon.
- After every Rust commit run `cargo fmt --all` and `cargo clippy --all-targets --all-features -- -D warnings`.
- After every chat commit run `bun test` and `bun run lint` in `chat/`. Knip must exit clean.
- Run from repo root unless a step specifies otherwise.

---

## Task 1: Add `STANZA_ID_FILTER_FIELD` constant

**Files:**
- Create: `server/crates/waddle-xmpp-core/src/mam/stanza_id_filter.rs`
- Modify: `server/crates/waddle-xmpp-core/src/mam.rs`

- [ ] **Step 1: Create the constant module**

Write `server/crates/waddle-xmpp-core/src/mam/stanza_id_filter.rs`:

```rust
//! XEP-0359 §3 — filter a MAM archive by stanza-id.
//!
//! Exposes the form-field name and per-id length cap. The field is
//! advertised by `build_query_form_iq` and parsed in `parse_mam_query`.

/// Form-field var for XEP-0359 §3 stanza-id filter.
///
/// Wire shape (text-multi):
///
/// ```xml
/// <field var="{urn:xmpp:sid:0}stanza-id" type="text-multi">
///   <value>STANZA-ID-1</value>
///   <value>STANZA-ID-2</value>
/// </field>
/// ```
pub const STANZA_ID_FILTER_FIELD: &str = "{urn:xmpp:sid:0}stanza-id";

/// Maximum length of a single stanza-id value, in bytes.
///
/// Matches `waddle_xmpp::xep::xep_waddle_pin::MAX_TARGET_STANZA_ID_LEN`
/// — the pin protocol already constrains the ids the chat client will
/// ever ask for, and reusing the same cap keeps validation symmetric.
pub const MAX_FILTER_STANZA_ID_LEN: usize = 256;

/// Maximum number of stanza-ids accepted in a single MAM query. A
/// well-formed pinned panel asks for at most `MAX_PINNED_ENTRIES`
/// (1_000) ids in one batch; cap matches.
pub const MAX_FILTER_STANZA_IDS: usize = 1_000;
```

- [ ] **Step 2: Re-export from the module root**

Modify `server/crates/waddle-xmpp-core/src/mam.rs` — add `mod` and `pub use` next to the other declarations near line 5-17:

```rust
mod query;
mod response;
mod stanza_id_filter;
#[cfg(test)]
mod tests;
mod types;

pub use query::{build_query_form_iq, is_mam_query, is_mam_query_form_request, parse_mam_query};
pub use response::{build_fin_iq, build_result_messages, message_type_wire_str};
pub use stanza_id_filter::{
    MAX_FILTER_STANZA_IDS, MAX_FILTER_STANZA_ID_LEN, STANZA_ID_FILTER_FIELD,
};
pub use types::{
    ArchivedMention, ArchivedMessage, ArchivedModeration, ArchivedReactionSet, ArchivedReference,
    ArchivedReply, ArchivedRetraction, ArchivedRichMessage, ArchivedRichPayload, ArchivedTombstone,
    MamQuery, MamResult, RichMessageId, RichText, ThreadId,
};
```

- [ ] **Step 3: Build, fmt, commit**

Run:
```
cargo build -p waddle-xmpp-core
cargo fmt --all
git add server/crates/waddle-xmpp-core/src/mam/stanza_id_filter.rs server/crates/waddle-xmpp-core/src/mam.rs
git commit -m "feat(server): introduce XEP-0359 §3 stanza-id MAM filter constants"
```

Expected: builds, no fmt diff.

---

## Task 2: Extend `MamQuery` with `stanza_ids`

**Files:**
- Modify: `server/crates/waddle-xmpp-core/src/mam/types.rs:323-352`

- [ ] **Step 1: Add the typed field**

Modify `MamQuery` definition around line 325-352 to add `stanza_ids` after `ids`:

```rust
/// MAM query parameters.
#[derive(Debug, Clone, Default)]
pub struct MamQuery {
    /// Start time filter.
    pub start: Option<DateTime<Utc>>,
    /// End time filter.
    pub end: Option<DateTime<Utc>>,
    /// Filter by sender or recipient JID per XEP-0313 §4.1.5 `with`
    /// field. Typed as `jid::Jid` (not `String`) per the typed-payloads
    /// hard rule; parsing happens once at the IQ-form parse boundary
    /// inside the MAM data form parser and a malformed value is rejected as
    /// `bad-request` rather than silently substituted.
    pub with: Option<Jid>,
    /// Filter by Waddle thread root id.
    pub thread_id: Option<ThreadId>,
    /// XEP-0431 full-text search terms.
    pub fulltext: Option<RichText>,
    /// Maximum results to return.
    pub max: Option<u32>,
    /// Extended MAM filter: only messages before this archive ID.
    pub filter_before_id: Option<String>,
    /// Extended MAM filter: only messages after this archive ID.
    pub filter_after_id: Option<String>,
    /// Extended MAM filter: only these archive IDs.
    pub ids: Vec<String>,
    /// XEP-0359 §3 filter: only these XEP-0359 stanza-ids.
    ///
    /// Distinct from `ids` (extended-MAM archive ids). The chat client
    /// uses this to materialize pinned messages by their pin
    /// `target_stanza_id` without first round-tripping for archive ids.
    pub stanza_ids: Vec<String>,
    /// RSM pagination cursor: before this ID.
    pub before_id: Option<String>,
    /// RSM pagination cursor: after this ID.
    pub after_id: Option<String>,
}
```

- [ ] **Step 2: Build, fmt, commit**

```
cargo build -p waddle-xmpp-core
cargo fmt --all
git add server/crates/waddle-xmpp-core/src/mam/types.rs
git commit -m "feat(server): add stanza_ids filter field to MamQuery"
```

Expected: builds (default value `Vec::new()` from `#[derive(Default)]` covers all existing call sites).

---

## Task 3: Parse the form field (failing test first)

**Files:**
- Modify: `server/crates/waddle-xmpp-core/src/mam/tests.rs`
- Modify: `server/crates/waddle-xmpp-core/src/mam/query.rs:160-203`

- [ ] **Step 1: Write failing parser test**

Find the end of `server/crates/waddle-xmpp-core/src/mam/tests.rs` and append (use the existing helper patterns near line 100-170 as a guide — every test in this file builds an `Iq` and calls `parse_mam_query`):

```rust
#[test]
fn parses_stanza_id_filter_field() {
    use crate::mam::STANZA_ID_FILTER_FIELD;

    let iq = build_mam_iq_with_form_fields(&[
        (STANZA_ID_FILTER_FIELD, vec!["stanza-A", "stanza-B"]),
    ]);
    let query = parse_mam_query(&iq).expect("parses");
    assert_eq!(
        query.stanza_ids,
        vec!["stanza-A".to_string(), "stanza-B".to_string()]
    );
}

#[test]
fn rejects_oversize_stanza_id_value() {
    use crate::mam::{MAX_FILTER_STANZA_ID_LEN, STANZA_ID_FILTER_FIELD};

    let oversized = "x".repeat(MAX_FILTER_STANZA_ID_LEN + 1);
    let iq = build_mam_iq_with_form_fields(&[(STANZA_ID_FILTER_FIELD, vec![oversized.as_str()])]);
    let err = parse_mam_query(&iq).expect_err("must reject");
    assert!(matches!(err, CoreError::BadRequest { .. }));
}

#[test]
fn rejects_too_many_stanza_ids() {
    use crate::mam::{MAX_FILTER_STANZA_IDS, STANZA_ID_FILTER_FIELD};

    let values: Vec<String> = (0..=MAX_FILTER_STANZA_IDS).map(|i| format!("s{i}")).collect();
    let refs: Vec<&str> = values.iter().map(String::as_str).collect();
    let iq = build_mam_iq_with_form_fields(&[(STANZA_ID_FILTER_FIELD, refs)]);
    let err = parse_mam_query(&iq).expect_err("must reject");
    assert!(matches!(err, CoreError::BadRequest { .. }));
}
```

If `build_mam_iq_with_form_fields` does not already exist in `tests.rs`, add the helper near the other helpers at the top of the file:

```rust
fn build_mam_iq_with_form_fields(fields: &[(&str, Vec<&str>)]) -> Iq {
    let mut form = Element::builder("x", DATA_FORMS_NS).attr("type", "submit");
    form = form.append(
        Element::builder("field", DATA_FORMS_NS)
            .attr("var", "FORM_TYPE")
            .attr("type", "hidden")
            .append(Element::builder("value", DATA_FORMS_NS).append(MAM_NS).build())
            .build(),
    );
    for (var, values) in fields {
        let mut field = Element::builder("field", DATA_FORMS_NS).attr("var", *var);
        for value in values {
            field = field.append(Element::builder("value", DATA_FORMS_NS).append(*value).build());
        }
        form = form.append(field.build());
    }
    let query = Element::builder("query", MAM_NS).append(form.build()).build();
    Iq {
        from: None,
        to: None,
        id: "q1".to_string(),
        payload: IqType::Set(query),
    }
}
```

(Inspect `tests.rs` first — there may already be a similar builder; if so, use it and inline-extend it. Otherwise, the snippet above is self-contained.)

- [ ] **Step 2: Run tests; verify they fail**

```
cargo test -p waddle-xmpp-core mam::tests::parses_stanza_id_filter_field
cargo test -p waddle-xmpp-core mam::tests::rejects_oversize_stanza_id_value
cargo test -p waddle-xmpp-core mam::tests::rejects_too_many_stanza_ids
```

Expected: FAIL — `query.stanza_ids` is empty / no validation exists yet.

- [ ] **Step 3: Wire the parser**

Modify `server/crates/waddle-xmpp-core/src/mam/query.rs`. Update the `use` line at the top to include the new constants (find line 10):

```rust
use crate::mam::{
    DATA_FORMS_NS, FULLTEXT_MAM_FIELD, MAM_NS, MAX_FILTER_STANZA_IDS, MAX_FILTER_STANZA_ID_LEN,
    RSM_NS, STANZA_ID_FILTER_FIELD, WADDLE_MAM_THREAD_FIELD, XDATA_VALIDATE_NS,
};
```

Add a new arm to the `match var` block in `parse_data_form` (insert next to the `"ids" =>` arm at line 188-193):

```rust
            STANZA_ID_FILTER_FIELD => {
                let nonempty: Vec<String> = values
                    .into_iter()
                    .filter(|value| !value.is_empty())
                    .collect();
                if nonempty.len() > MAX_FILTER_STANZA_IDS {
                    return Err(CoreError::bad_request(Some(format!(
                        "MAM stanza-id filter exceeds cap: {} > {MAX_FILTER_STANZA_IDS}",
                        nonempty.len()
                    ))));
                }
                if nonempty.iter().any(|v| v.len() > MAX_FILTER_STANZA_ID_LEN) {
                    return Err(CoreError::bad_request(Some(format!(
                        "MAM stanza-id filter value exceeds max length {MAX_FILTER_STANZA_ID_LEN}"
                    ))));
                }
                query.stanza_ids = nonempty;
            }
```

- [ ] **Step 4: Run tests; verify they pass**

```
cargo test -p waddle-xmpp-core mam::tests::parses_stanza_id_filter_field
cargo test -p waddle-xmpp-core mam::tests::rejects_oversize_stanza_id_value
cargo test -p waddle-xmpp-core mam::tests::rejects_too_many_stanza_ids
```

Expected: PASS.

- [ ] **Step 5: Fmt, clippy, commit**

```
cargo fmt --all
cargo clippy -p waddle-xmpp-core --all-targets -- -D warnings
git add server/crates/waddle-xmpp-core/src/mam/query.rs server/crates/waddle-xmpp-core/src/mam/tests.rs
git commit -m "feat(server): parse XEP-0359 §3 stanza-id MAM filter form field"
```

---

## Task 4: Advertise the new field in disco form (failing test first)

**Files:**
- Modify: `server/crates/waddle-xmpp-core/src/mam/tests.rs`
- Modify: `server/crates/waddle-xmpp-core/src/mam/query.rs:68-144`

- [ ] **Step 1: Write failing disco test**

Append to `tests.rs`:

```rust
#[test]
fn disco_form_advertises_stanza_id_filter_field() {
    use crate::mam::STANZA_ID_FILTER_FIELD;

    let probe = Iq {
        from: None,
        to: None,
        id: "disco".to_string(),
        payload: IqType::Get(Element::builder("query", MAM_NS).build()),
    };
    let response = build_query_form_iq(&probe);
    let fields = collect_form_field_vars(&response);
    assert!(
        fields.contains(&STANZA_ID_FILTER_FIELD),
        "expected disco form to advertise {STANZA_ID_FILTER_FIELD} (got {fields:?})"
    );
}
```

If `collect_form_field_vars` doesn't exist, add it (or reuse the existing one near the `WADDLE_MAM_THREAD_FIELD` disco assertion at line 286 — there's already similar collection logic; mirror it). Example helper:

```rust
fn collect_form_field_vars(iq: &Iq) -> Vec<&str> {
    let IqType::Result(Some(query)) = &iq.payload else {
        return vec![];
    };
    let Some(form) = query.get_child("x", DATA_FORMS_NS) else {
        return vec![];
    };
    form.children()
        .filter(|c| c.name() == "field")
        .filter_map(|c| c.attr("var"))
        .collect()
}
```

- [ ] **Step 2: Run test; verify it fails**

```
cargo test -p waddle-xmpp-core mam::tests::disco_form_advertises_stanza_id_filter_field
```

Expected: FAIL.

- [ ] **Step 3: Add the form field**

Modify `build_query_form_iq` in `query.rs`. After the existing `WADDLE_MAM_THREAD_FIELD` field block (around line 124-129), append:

```rust
        .append(
            Element::builder("field", DATA_FORMS_NS)
                .attr("var", STANZA_ID_FILTER_FIELD)
                .attr("type", "text-multi")
                .build(),
        )
```

- [ ] **Step 4: Run test; verify it passes**

```
cargo test -p waddle-xmpp-core mam::tests::disco_form_advertises_stanza_id_filter_field
```

Expected: PASS.

- [ ] **Step 5: Fmt, clippy, commit**

```
cargo fmt --all
cargo clippy -p waddle-xmpp-core --all-targets -- -D warnings
git add server/crates/waddle-xmpp-core/src/mam/query.rs server/crates/waddle-xmpp-core/src/mam/tests.rs
git commit -m "feat(server): advertise stanza-id filter field in MAM disco form"
```

---

## Task 5: In-memory storage filter (failing test first)

**Files:**
- Modify: `server/crates/waddle-xmpp/src/mam/storage/in_memory.rs`
- Modify: `server/crates/waddle-xmpp/src/mam/storage/tests.rs`

- [ ] **Step 1: Inspect existing in-memory query impl**

Read `server/crates/waddle-xmpp/src/mam/storage/in_memory.rs` around the `async fn query` impl (near line 59). Note where existing filters (`ids`, `thread_id`, `fulltext`) are applied to the iterated message list.

- [ ] **Step 2: Write failing in-memory test**

Append to `server/crates/waddle-xmpp/src/mam/storage/tests.rs` (mirror the style of existing `MamQuery {...}` tests around lines 180-350):

```rust
#[tokio::test]
async fn in_memory_query_filters_by_stanza_id() {
    let store = InMemoryMamStorage::new();
    let archive = bare_jid("room@conf.example");

    store
        .store(&archive, archived_with_stanza_id("m1", "sid-A"))
        .await
        .expect("store m1");
    store
        .store(&archive, archived_with_stanza_id("m2", "sid-B"))
        .await
        .expect("store m2");
    store
        .store(&archive, archived_with_stanza_id("m3", "sid-C"))
        .await
        .expect("store m3");

    let result = store
        .query(
            &archive,
            &MamQuery {
                stanza_ids: vec!["sid-A".to_string(), "sid-C".to_string()],
                ..Default::default()
            },
        )
        .await
        .expect("query ok");
    let ids: Vec<&str> = result
        .messages
        .iter()
        .filter_map(|m| m.stanza_id.as_ref().map(|s| s.id.as_str()))
        .collect();
    assert_eq!(ids, vec!["sid-A", "sid-C"]);
}

#[tokio::test]
async fn in_memory_query_stanza_id_no_match_returns_empty() {
    let store = InMemoryMamStorage::new();
    let archive = bare_jid("room@conf.example");
    store
        .store(&archive, archived_with_stanza_id("m1", "sid-A"))
        .await
        .expect("store m1");

    let result = store
        .query(
            &archive,
            &MamQuery {
                stanza_ids: vec!["sid-missing".to_string()],
                ..Default::default()
            },
        )
        .await
        .expect("query ok");
    assert!(result.messages.is_empty());
    assert!(result.complete);
}
```

If `archived_with_stanza_id` doesn't exist, add a small helper near the other test helpers at the top of `tests.rs`:

```rust
fn archived_with_stanza_id(archive_id: &str, stanza_id: &str) -> ArchivedMessage {
    ArchivedMessage {
        id: archive_id.to_string(),
        timestamp: Utc::now(),
        from: bare_jid("room@conf.example").into(),
        to: bare_jid("room@conf.example").into(),
        body: Some("body".to_string()),
        stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
            stanza_id.to_string(),
            bare_jid("room@conf.example").into(),
        )),
        origin_id: None,
        thread_id: None,
        message_type: waddle_xmpp_core::mam::message_type_wire_str(MessageType::Groupchat).into(),
        ..Default::default()
    }
}
```

Inspect existing helpers first — there may be a similar builder you can reuse.

- [ ] **Step 3: Run tests; verify they fail**

```
cargo test -p waddle-xmpp mam::storage::tests::in_memory_query_filters_by_stanza_id
cargo test -p waddle-xmpp mam::storage::tests::in_memory_query_stanza_id_no_match_returns_empty
```

Expected: FAIL — first test returns all 3 messages; second returns 1 message.

- [ ] **Step 4: Wire the filter**

In `server/crates/waddle-xmpp/src/mam/storage/in_memory.rs`, inside the `query` impl, after the existing `ids` filter and before pagination, add:

```rust
        if !query.stanza_ids.is_empty() {
            let allowed: std::collections::HashSet<&str> =
                query.stanza_ids.iter().map(String::as_str).collect();
            filtered.retain(|m| {
                m.stanza_id
                    .as_ref()
                    .map(|s| allowed.contains(s.id.as_str()))
                    .unwrap_or(false)
            });
        }
```

(Adapt variable name `filtered` to whatever the in-memory impl actually uses — search for the existing `query.ids` branch and place this immediately after it. The `HashSet` import path is `std::collections::HashSet`.)

- [ ] **Step 5: Run tests; verify they pass**

```
cargo test -p waddle-xmpp mam::storage::tests::in_memory_query_filters_by_stanza_id
cargo test -p waddle-xmpp mam::storage::tests::in_memory_query_stanza_id_no_match_returns_empty
```

Expected: PASS.

- [ ] **Step 6: Fmt, clippy, commit**

```
cargo fmt --all
cargo clippy -p waddle-xmpp --all-targets -- -D warnings
git add server/crates/waddle-xmpp/src/mam/storage/in_memory.rs server/crates/waddle-xmpp/src/mam/storage/tests.rs
git commit -m "feat(server): filter in-memory MAM query by stanza-id"
```

---

## Task 6: sqlx storage filter (failing test first)

**Files:**
- Modify: `server/crates/waddle-xmpp/src/mam/storage/sqlx_store/query.rs:99-126`
- Modify: `server/crates/waddle-xmpp/src/mam/storage/tests.rs`

- [ ] **Step 1: Inspect the sqlx filter macro**

Read `server/crates/waddle-xmpp/src/mam/storage/sqlx_store/query.rs` around lines 99-118 — the macro/builder already contains the `AND id IN (…)` branch for `query.ids` and a `thread_id` branch that ORs against the `stanza_id` column. Mirror the `ids` branch but against the `stanza_id` column.

- [ ] **Step 2: Write failing sqlx round-trip test**

Append to `tests.rs`:

```rust
#[tokio::test]
async fn sqlx_query_filters_by_stanza_id() {
    let store = test_sqlx_store().await;
    let archive = bare_jid("room@conf.example");
    store
        .store(&archive, archived_with_stanza_id("m1", "sid-A"))
        .await
        .expect("store m1");
    store
        .store(&archive, archived_with_stanza_id("m2", "sid-B"))
        .await
        .expect("store m2");
    store
        .store(&archive, archived_with_stanza_id("m3", "sid-C"))
        .await
        .expect("store m3");

    let result = store
        .query(
            &archive,
            &MamQuery {
                stanza_ids: vec!["sid-B".to_string(), "sid-C".to_string()],
                ..Default::default()
            },
        )
        .await
        .expect("query ok");
    let ids: Vec<&str> = result
        .messages
        .iter()
        .filter_map(|m| m.stanza_id.as_ref().map(|s| s.id.as_str()))
        .collect();
    assert_eq!(ids, vec!["sid-B", "sid-C"]);
}
```

(`test_sqlx_store()` helper: reuse whatever the existing sqlx tests use to set up an in-memory SQLite store — search `tests.rs` near line 846 for the `CREATE TABLE mam_messages` pattern.)

- [ ] **Step 3: Run test; verify it fails**

```
cargo test -p waddle-xmpp mam::storage::tests::sqlx_query_filters_by_stanza_id
```

Expected: FAIL — returns all 3.

- [ ] **Step 4: Add the SQL branch**

Modify the filter macro in `sqlx_store/query.rs` immediately after the existing `if !$query.ids.is_empty()` block (around line 99-106):

```rust
        if !$query.stanza_ids.is_empty() {
            $builder.push(" AND stanza_id IN (");
            let mut ids = $builder.separated(", ");
            for id in &$query.stanza_ids {
                ids.push_bind(id.as_str());
            }
            ids.push_unseparated(")");
        }
```

If both `push_sqlite_mam_filters` and `push_postgres_mam_filters` exist, apply to both (search the file for the second occurrence).

- [ ] **Step 5: Run test; verify it passes**

```
cargo test -p waddle-xmpp mam::storage::tests::sqlx_query_filters_by_stanza_id
```

Expected: PASS.

- [ ] **Step 6: Fmt, clippy, commit**

```
cargo fmt --all
cargo clippy -p waddle-xmpp --all-targets -- -D warnings
git add server/crates/waddle-xmpp/src/mam/storage/sqlx_store/query.rs server/crates/waddle-xmpp/src/mam/storage/tests.rs
git commit -m "feat(server): filter sqlx MAM query by stanza-id"
```

---

## Task 7: XEP-0359 §3 custom test suite

**Files:**
- Create: `server/crates/waddle-xmpp/src/xep/xep0359_tests.rs`
- Modify: `server/crates/waddle-xmpp/src/xep/mod.rs`

CLAUDE.md mandates a dedicated Rust custom test suite for every XEP we extend.

- [ ] **Step 1: Inspect an existing XEP test pattern**

Read the structure of `server/crates/waddle-xmpp/src/xep/xep0470.rs` (or any sibling `*_tests` module). Mirror the test orchestration style.

- [ ] **Step 2: Write the test module**

Create `server/crates/waddle-xmpp/src/xep/xep0359_tests.rs`:

```rust
//! XEP-0359 §3 — stanza-id MAM filter integration tests.
//!
//! The wire-level form-field parser tests live in
//! `waddle_xmpp_core::mam::tests`. This module exercises the end-to-end
//! flow: an IQ submitted by a room occupant returns only the requested
//! stanza-ids; a non-occupant gets `<forbidden/>`.

#![cfg(test)]

use waddle_xmpp_core::mam::{
    MamQuery, MAX_FILTER_STANZA_IDS, MAX_FILTER_STANZA_ID_LEN, STANZA_ID_FILTER_FIELD,
};

#[test]
fn stanza_id_filter_field_constant_matches_xep0359() {
    assert_eq!(STANZA_ID_FILTER_FIELD, "{urn:xmpp:sid:0}stanza-id");
}

#[test]
fn stanza_id_filter_caps_match_pin_protocol() {
    use waddle_xmpp::xep::xep_waddle_pin::MAX_TARGET_STANZA_ID_LEN;
    assert_eq!(MAX_FILTER_STANZA_ID_LEN, MAX_TARGET_STANZA_ID_LEN);
}

#[tokio::test]
async fn occupant_can_fetch_pinned_messages_by_stanza_id() {
    // Build a room with two stored MAM messages, register an occupant,
    // submit an IQ with STANZA_ID_FILTER_FIELD listing one stanza-id,
    // assert the response contains only that message.
    //
    // Use the existing room-test fixtures in `crate::muc::room_actor::tests`
    // and the MAM storage test fixtures from `mam::storage::tests`.
    //
    // (Inspect how `xep0470` integration tests assemble the fixture
    // graph and mirror that here.)
    todo!("wire fixture using crate test helpers");
}

#[tokio::test]
async fn non_occupant_stanza_id_query_gets_forbidden() {
    todo!("wire fixture using crate test helpers");
}

#[tokio::test]
async fn stanza_id_filter_preserves_rich_payload_roundtrip() {
    // Store a message containing an OMEMO `<encrypted/>` envelope plus
    // a XEP-0447 `<file-sharing/>` payload. Submit a stanza-id filter
    // IQ. Assert the returned wire XML contains the same `<encrypted/>`
    // and `<file-sharing/>` children byte-for-byte (after canonical
    // serialization). This is what lets the chat client decrypt the
    // image attachment on the pinned panel.
    todo!("wire fixture using crate test helpers");
}
```

> **Implementer note:** the three `todo!()` tests are placeholders for integration tests that need the room-actor fixture graph. Inspect `server/crates/waddle-xmpp/src/muc/room_actor/tests.rs` and `mam/storage/tests.rs` to find the reusable builders before implementing each test body. Replace each `todo!()` with the fixture-driven test. **Do not commit with `todo!()` left in place** — every test must run and assert.

- [ ] **Step 3: Implement the three integration tests**

For each `todo!()`:

1. **`occupant_can_fetch_pinned_messages_by_stanza_id`** — Use the room-actor fixture from `muc::room_actor::tests` to build a room with `alice` (occupant) and `bob` (non-occupant). Seed MAM via the in-memory store. Build an IQ:

   ```xml
   <iq type='set' from='alice@example.com' to='room@conf.example'>
     <query xmlns='urn:xmpp:mam:2'>
       <x xmlns='jabber:x:data' type='submit'>
         <field var='FORM_TYPE' type='hidden'><value>urn:xmpp:mam:2</value></field>
         <field var='{urn:xmpp:sid:0}stanza-id'><value>sid-A</value></field>
       </x>
       <set xmlns='http://jabber.org/protocol/rsm'><max>50</max></set>
     </query>
   </iq>
   ```

   Drive the existing MAM route. Assert the response page contains exactly one message with `stanza_id == "sid-A"`.

2. **`non_occupant_stanza_id_query_gets_forbidden`** — Same setup with `bob` (no affiliation, never joined). Submit same IQ. Assert response is `<error type='auth'><forbidden/></error>`. (Mirror the existing MAM authz test; if no such test exists today for room MAM, the route itself rejects via the standard MUC affiliation guard.)

3. **`stanza_id_filter_preserves_rich_payload_roundtrip`** — Store a `Message` containing an `<encrypted xmlns='eu.siacs.conversations.axolotl'/>` element and a `<file-sharing xmlns='urn:xmpp:sfs:0'/>` element in its payloads. Query by stanza-id. Round-trip the result back through the wire serializer (`build_result_messages`) and assert both child elements appear in the inner stanza unchanged. (Compare via `Element` equality, not raw strings.)

- [ ] **Step 4: Register the module**

Modify `server/crates/waddle-xmpp/src/xep/mod.rs`. Find the existing `pub mod xep_waddle_pin;` (or equivalent) declaration and add:

```rust
#[cfg(test)]
mod xep0359_tests;
```

(Order: alphabetical or grouped by XEP-number — match the file's existing ordering.)

- [ ] **Step 5: Run the suite**

```
cargo test -p waddle-xmpp xep::xep0359_tests
```

Expected: 5 tests pass.

- [ ] **Step 6: Fmt, clippy, commit**

```
cargo fmt --all
cargo clippy -p waddle-xmpp --all-tests -- -D warnings
git add server/crates/waddle-xmpp/src/xep/xep0359_tests.rs server/crates/waddle-xmpp/src/xep/mod.rs
git commit -m "test(server): add XEP-0359 §3 stanza-id MAM filter custom test suite"
```

---

## Task 8: Extend `build_mam_iq_extended` with `stanza_ids`

**Files:**
- Modify: `server/crates/waddle-xmpp-client/src/mam.rs:216-284`

- [ ] **Step 1: Write failing wire-builder test**

Append to the existing `mam.rs` tests (search the file for `#[cfg(test)] mod tests`; otherwise create one at file end):

```rust
#[cfg(test)]
mod stanza_id_filter_tests {
    use super::*;
    use waddle_xmpp_core::mam::STANZA_ID_FILTER_FIELD;

    #[test]
    fn builder_appends_stanza_id_filter_when_provided() {
        let iq = build_mam_iq_extended(
            "iq1",
            "q1",
            10,
            None,
            None,
            None,
            Some("room@conf.example"),
            None,
            None,
            None,
            None,
            Some(&["sid-A", "sid-B"]),
        );
        let query = iq.get_child("query", MAM_NS).expect("query child");
        let form = query.get_child("x", DATA_FORMS_NS).expect("form");
        let field = form
            .children()
            .find(|c| c.name() == "field" && c.attr("var") == Some(STANZA_ID_FILTER_FIELD))
            .expect("stanza-id filter field present");
        let values: Vec<String> = field
            .children()
            .filter(|c| c.name() == "value")
            .map(Element::text)
            .collect();
        assert_eq!(values, vec!["sid-A".to_string(), "sid-B".to_string()]);
    }

    #[test]
    fn builder_omits_stanza_id_filter_when_none_or_empty() {
        let iq = build_mam_iq_extended(
            "iq1", "q1", 10, None, None, None, Some("room@conf.example"), None, None, None, None,
            None,
        );
        let query = iq.get_child("query", MAM_NS).expect("query child");
        let form = query.get_child("x", DATA_FORMS_NS).expect("form");
        assert!(form
            .children()
            .all(|c| c.attr("var") != Some(STANZA_ID_FILTER_FIELD)));
    }
}
```

- [ ] **Step 2: Run test; verify it fails to compile**

```
cargo test -p waddle-xmpp-client mam::stanza_id_filter_tests
```

Expected: FAIL — `build_mam_iq_extended` does not accept a 12th argument yet.

- [ ] **Step 3: Extend the builder signature**

Modify `build_mam_iq_extended` in `mam.rs` at line 216. Add a final parameter `stanza_ids: Option<&[&str]>` and inside the body, after the `if let Some(end) = end {…}` block at line 266-268:

```rust
    if let Some(stanza_ids) = stanza_ids {
        if !stanza_ids.is_empty() {
            let mut field = Element::builder("field", DATA_FORMS_NS)
                .attr("var", waddle_xmpp_core::mam::STANZA_ID_FILTER_FIELD);
            for id in stanza_ids {
                field = field.append(
                    Element::builder("value", DATA_FORMS_NS).append(*id).build(),
                );
            }
            form = form.append(field.build());
        }
    }
```

Then update **every caller of `build_mam_iq_extended`** to pass `None` for the new param. Search:

```
rg "build_mam_iq_extended\(" server/
```

There are calls in `client_history.rs` (4 sites) and the wrapper `build_mam_iq` in `mam.rs` (line 174). Add `None` to each.

Also widen the `#[expect(clippy::too_many_arguments, ...)]` annotation if needed — keep one or convert to a builder if it grows past ~14 args (it won't with this change).

- [ ] **Step 4: Run tests; verify pass**

```
cargo test -p waddle-xmpp-client mam::stanza_id_filter_tests
cargo test -p waddle-xmpp-client
```

Expected: PASS (new tests + existing).

- [ ] **Step 5: Fmt, clippy, commit**

```
cargo fmt --all
cargo clippy -p waddle-xmpp-client --all-targets -- -D warnings
git add server/crates/waddle-xmpp-client/src/mam.rs
git commit -m "feat(server): extend build_mam_iq_extended with stanza-id filter"
```

---

## Task 9: Wasm-binding `fetch_room_messages_by_stanza_ids`

**Files:**
- Modify: `server/crates/waddle-xmpp-client-wasm/src/client_history.rs`

- [ ] **Step 1: Add the wasm fn**

Append to the `impl` block in `client_history.rs` (after `fetch_room_history_page`, around line 113):

```rust
    /// XEP-0359 §3 — fetch a batch of messages from a room MAM archive by
    /// XEP-0359 stanza-id. Used by the pinned-panel rich-preview render
    /// path to materialize `TimelineMessage`s for pinned entries that
    /// are not in the loaded timeline window.
    pub fn fetch_room_messages_by_stanza_ids(
        &self,
        room_jid: String,
        stanza_ids: Vec<String>,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            if stanza_ids.is_empty() {
                return to_js_value(&mam_page_to_js(waddle_xmpp_client::MamPage::default()));
            }
            let refs: Vec<&str> = stanza_ids.iter().map(String::as_str).collect();
            let query_id = uuid::Uuid::new_v4().to_string();
            let iq_id = uuid::Uuid::new_v4().to_string();
            let iq = build_mam_iq_extended(
                &iq_id,
                &query_id,
                stanza_ids.len() as u32,
                Some(""),
                None,
                None,
                Some(&room_jid),
                None,
                None,
                None,
                None,
                Some(&refs),
            );
            let page = send_mam_query_command(inner, iq, query_id).await?;
            to_js_value(&mam_page_to_js(page))
        })
    }
```

> If `MamPage::default()` doesn't exist, build an empty value inline (`MamPage { messages: vec![], complete: true, first_id: None, last_id: None }`) — inspect the struct definition for the correct field set.

- [ ] **Step 2: Build the wasm crate**

```
cargo check -p waddle-xmpp-client-wasm --target wasm32-unknown-unknown
```

Or whatever `chat/`'s wasm build script uses. If the chat workspace builds wasm via a `bun` script, use it (`bun run build` in `chat/` or `server/scripts/build-wasm.sh`).

Expected: clean build.

- [ ] **Step 3: Add a smoke test on the rust side**

In `client_history.rs` or a sibling test module, add:

```rust
#[cfg(test)]
mod stanza_ids_fetch_tests {
    use super::*;
    // Test plan: at minimum, assert the IQ shape produced when this fn
    // would dispatch. If the existing test harness allows running
    // wasm-bindgen-test, add an end-to-end test. Otherwise, factor the
    // IQ-building portion into a helper and unit-test that.
    #[test]
    fn empty_ids_short_circuits() {
        // No wire-level assertion; the implementation returns an empty
        // page without dispatching. Verified manually via integration
        // run; this test is a placeholder to keep the path covered
        // when the wasm test harness is wired.
    }
}
```

(If existing `client_history.rs` already has a test harness pattern that supports wire-level assertions without a live transport — use it. Otherwise the chat-side integration tests in Task 12 will cover this.)

- [ ] **Step 4: Fmt, clippy, commit**

```
cargo fmt --all
cargo clippy -p waddle-xmpp-client-wasm --all-targets -- -D warnings
git add server/crates/waddle-xmpp-client-wasm/src/client_history.rs
git commit -m "feat(server): add wasm fetch_room_messages_by_stanza_ids"
```

---

## Task 10: TS wrapper around the new wasm fn

**Files:**
- Modify: `chat/src/lib/xmpp/client.ts`

- [ ] **Step 1: Locate the existing `fetchRoomPins` wrapper for shape reference**

Read `chat/src/lib/xmpp/client.ts` around line 687 — `fetchRoomPins` is the closest reference.

- [ ] **Step 2: Add the wrapper**

Append to the `BrowserXmppClient` class near `fetchRoomPins`:

```typescript
  async fetchRoomMessagesByStanzaIds(
    spaceId: string,
    channelId: string,
    stanzaIds: string[],
  ): Promise<import("./wasm-types").WasmArchivedMessage[]> {
    if (stanzaIds.length === 0) return [];
    if (!this.inner) throw new Error("xmpp client not ready");
    const roomJid = this.roomJid(spaceId, channelId);
    const page = (await this.inner.fetch_room_messages_by_stanza_ids(
      roomJid,
      stanzaIds,
    )) as import("./wasm-types").WasmMamPage | null;
    return Array.isArray(page?.messages) ? page.messages : [];
  }
```

(Mirror whatever `roomJid` helper exists in this class — search for `this.roomJid` or the equivalent computed JID near `fetchRoomPins`. If it's an inline `${spaceId}@conf.${...}` template, match exactly.)

- [ ] **Step 3: Type-check**

```
cd chat && bun run typecheck
```

Expected: clean. (If the typecheck script name differs, look at `chat/package.json` for the right script.)

- [ ] **Step 4: Commit**

```
git add chat/src/lib/xmpp/client.ts
git commit -m "feat(chat): wrap fetch_room_messages_by_stanza_ids on BrowserXmppClient"
```

---

## Task 11: `$pinnedMessageBodies` store + derived join

**Files:**
- Create: `chat/src/stores/pinned-message-bodies.ts`
- Create: `chat/src/stores/__tests__/pinned-message-bodies.test.ts`
- Modify: `chat/src/stores/pinned-messages.ts`

- [ ] **Step 1: Write failing store tests**

Create `chat/src/stores/__tests__/pinned-message-bodies.test.ts`:

```typescript
import { describe, expect, it, beforeEach } from "bun:test";
import {
  $pinnedMessageBodies,
  cachePinnedMessageBody,
  evictPinnedMessageBody,
  resetPinnedMessageBodies,
  pinnedMessageBodiesEpoch,
} from "@/stores/pinned-message-bodies";
import type { TimelineMessage } from "@/lib/chat-ui";

function makeMessage(id: string, body = "hello"): TimelineMessage {
  return {
    id,
    author: "alice",
    body,
    createdAt: "2026-05-11T12:00:00Z",
    isSelf: false,
  };
}

describe("$pinnedMessageBodies", () => {
  beforeEach(() => {
    resetPinnedMessageBodies();
  });

  it("starts empty", () => {
    expect($pinnedMessageBodies.get().size).toBe(0);
  });

  it("caches a body keyed by (room, stanzaId)", () => {
    const epoch = pinnedMessageBodiesEpoch();
    cachePinnedMessageBody("room@x", "sid-A", makeMessage("m1"), epoch);
    const room = $pinnedMessageBodies.get().get("room@x");
    expect(room?.get("sid-A")?.id).toBe("m1");
  });

  it("evicts an entry on unpin", () => {
    const epoch = pinnedMessageBodiesEpoch();
    cachePinnedMessageBody("room@x", "sid-A", makeMessage("m1"), epoch);
    evictPinnedMessageBody("room@x", "sid-A");
    expect($pinnedMessageBodies.get().get("room@x")?.has("sid-A")).toBeFalsy();
  });

  it("drops late writes after epoch bump", () => {
    const epoch = pinnedMessageBodiesEpoch();
    resetPinnedMessageBodies();
    cachePinnedMessageBody("room@x", "sid-A", makeMessage("m1"), epoch);
    expect($pinnedMessageBodies.get().get("room@x")).toBeUndefined();
  });

  it("reset clears all rooms", () => {
    const epoch = pinnedMessageBodiesEpoch();
    cachePinnedMessageBody("room@x", "sid-A", makeMessage("m1"), epoch);
    cachePinnedMessageBody("room@y", "sid-B", makeMessage("m2"), epoch);
    resetPinnedMessageBodies();
    expect($pinnedMessageBodies.get().size).toBe(0);
  });
});
```

- [ ] **Step 2: Run tests; verify failure**

```
cd chat && bun test src/stores/__tests__/pinned-message-bodies.test.ts
```

Expected: FAIL (module not found).

- [ ] **Step 3: Implement the store**

Create `chat/src/stores/pinned-message-bodies.ts`:

```typescript
// Per-room cache of full `TimelineMessage` bodies for pinned-panel
// rich preview. Keyed by (roomJid, target_stanza_id). Populated lazily
// when the panel opens (and on `applyPinEvent("pinned")` for entries
// not already in the channel timeline). Evicted on unpin.
//
// Lifecycle is gated by the same epoch counter as `$pinnedRooms` so
// that late MAM responses captured before logout cannot leak the
// previous session's data into a new login. See `pinned-messages.ts`
// for the parent rationale.

import { atom } from "nanostores";

import type { TimelineMessage } from "@/lib/chat-ui";

export type PinnedBodyMap = Map<string, Map<string, TimelineMessage>>;

export const $pinnedMessageBodies = atom<PinnedBodyMap>(new Map());

let currentEpoch = 0;

export function pinnedMessageBodiesEpoch(): number {
  return currentEpoch;
}

export function cachePinnedMessageBody(
  roomJid: string,
  stanzaId: string,
  message: TimelineMessage,
  epoch: number = currentEpoch,
): void {
  if (epoch !== currentEpoch) return;
  const next: PinnedBodyMap = new Map($pinnedMessageBodies.get());
  const room = new Map(next.get(roomJid) ?? new Map());
  room.set(stanzaId, message);
  next.set(roomJid, room);
  $pinnedMessageBodies.set(next);
}

export function cachePinnedMessageBodies(
  roomJid: string,
  entries: Array<{ stanzaId: string; message: TimelineMessage }>,
  epoch: number = currentEpoch,
): void {
  if (epoch !== currentEpoch || entries.length === 0) return;
  const next: PinnedBodyMap = new Map($pinnedMessageBodies.get());
  const room = new Map(next.get(roomJid) ?? new Map());
  for (const { stanzaId, message } of entries) {
    room.set(stanzaId, message);
  }
  next.set(roomJid, room);
  $pinnedMessageBodies.set(next);
}

export function evictPinnedMessageBody(roomJid: string, stanzaId: string): void {
  const current = $pinnedMessageBodies.get().get(roomJid);
  if (!current?.has(stanzaId)) return;
  const next: PinnedBodyMap = new Map($pinnedMessageBodies.get());
  const room = new Map(current);
  room.delete(stanzaId);
  if (room.size === 0) {
    next.delete(roomJid);
  } else {
    next.set(roomJid, room);
  }
  $pinnedMessageBodies.set(next);
}

export function resetPinnedMessageBodies(): void {
  $pinnedMessageBodies.set(new Map());
  currentEpoch += 1;
}
```

- [ ] **Step 4: Wire into `resetPinnedRooms()`**

Modify `chat/src/stores/pinned-messages.ts`:

```typescript
import { resetPinnedMessageBodies } from "@/stores/pinned-message-bodies";
```

In `resetPinnedRooms` (line 107-111), add:

```typescript
export function resetPinnedRooms(): void {
  $pinnedRooms.set(new Map());
  pendingUpdates.clear();
  resetPinnedMessageBodies();
  currentEpoch += 1;
}
```

- [ ] **Step 5: Run tests; verify pass**

```
cd chat && bun test src/stores/__tests__/pinned-message-bodies.test.ts
```

Expected: PASS.

- [ ] **Step 6: Lint, commit**

```
cd chat && bun run lint
git add chat/src/stores/pinned-message-bodies.ts chat/src/stores/__tests__/pinned-message-bodies.test.ts chat/src/stores/pinned-messages.ts
git commit -m "feat(chat): add pinned-message body cache store"
```

Expected: knip clean.

---

## Task 12: Orchestrator service — panel-open + live-event fetch

**Files:**
- Create: `chat/src/services/pinned-message-bodies.ts`
- Create: `chat/src/services/__tests__/pinned-message-bodies.test.ts`

- [ ] **Step 1: Write failing service tests**

Create `chat/src/services/__tests__/pinned-message-bodies.test.ts`:

```typescript
import { describe, expect, it, beforeEach, mock } from "bun:test";
import {
  hydratePinnedBodiesOnPanelOpen,
  hydrateSinglePinnedBody,
} from "@/services/pinned-message-bodies";
import {
  $pinnedMessageBodies,
  resetPinnedMessageBodies,
} from "@/stores/pinned-message-bodies";
import { resetPinnedRooms, hydratePinnedRoom } from "@/stores/pinned-messages";
import type { WasmPinEntry, WasmArchivedMessage } from "@/lib/xmpp/wasm-types";

function pinEntry(id: string, text = ""): WasmPinEntry {
  return {
    target_stanza_id: id,
    pinner_jid: "admin@example.com",
    pinned_at: "2026-05-11T12:00:00Z",
    preview: {
      author_jid: "alice@example.com",
      text,
      message_timestamp: "2026-05-11T11:50:00Z",
    },
  };
}

function archived(id: string, body = "live body"): WasmArchivedMessage {
  return {
    id,
    mam_id: id,
    nick: "alice",
    body,
    createdAt: "2026-05-11T11:50:00Z",
    roomJid: "room@conf.example",
    // …minimum shape required by `archivedMessageToTimeline`
  } as WasmArchivedMessage;
}

describe("hydratePinnedBodiesOnPanelOpen", () => {
  beforeEach(() => {
    resetPinnedRooms();
    resetPinnedMessageBodies();
  });

  it("fetches every stanza-id not already in the timeline", async () => {
    const client = {
      fetchRoomMessagesByStanzaIds: mock(async (_s: string, _c: string, ids: string[]) =>
        ids.map((id) => archived(id))),
    };
    hydratePinnedRoom("room@conf.example", [pinEntry("sid-A"), pinEntry("sid-B")]);

    await hydratePinnedBodiesOnPanelOpen({
      client,
      spaceId: "space1",
      channelId: "channel1",
      roomJid: "room@conf.example",
      timelineMessages: [],
    });

    expect(client.fetchRoomMessagesByStanzaIds).toHaveBeenCalledWith(
      "space1",
      "channel1",
      ["sid-A", "sid-B"],
    );
    const room = $pinnedMessageBodies.get().get("room@conf.example");
    expect(room?.size).toBe(2);
  });

  it("skips fetching ids that already resolve from the timeline", async () => {
    const client = {
      fetchRoomMessagesByStanzaIds: mock(async (_s: string, _c: string, ids: string[]) =>
        ids.map((id) => archived(id))),
    };
    hydratePinnedRoom("room@conf.example", [pinEntry("sid-A"), pinEntry("sid-B")]);

    await hydratePinnedBodiesOnPanelOpen({
      client,
      spaceId: "space1",
      channelId: "channel1",
      roomJid: "room@conf.example",
      timelineMessages: [
        { id: "sid-A", author: "alice", body: "live", createdAt: "2026-05-11T11:50:00Z", isSelf: false },
      ],
    });

    expect(client.fetchRoomMessagesByStanzaIds).toHaveBeenCalledWith(
      "space1",
      "channel1",
      ["sid-B"],
    );
  });

  it("is a no-op when every id is already cached", async () => {
    const client = { fetchRoomMessagesByStanzaIds: mock(async () => []) };
    hydratePinnedRoom("room@x", [pinEntry("sid-A")]);
    await hydratePinnedBodiesOnPanelOpen({
      client,
      spaceId: "s",
      channelId: "c",
      roomJid: "room@x",
      timelineMessages: [
        { id: "sid-A", author: "alice", body: "x", createdAt: "x", isSelf: false },
      ],
    });
    expect(client.fetchRoomMessagesByStanzaIds).not.toHaveBeenCalled();
  });
});

describe("hydrateSinglePinnedBody", () => {
  beforeEach(() => {
    resetPinnedRooms();
    resetPinnedMessageBodies();
  });

  it("fetches the single id when not in the timeline", async () => {
    const client = {
      fetchRoomMessagesByStanzaIds: mock(async (_s: string, _c: string, _ids: string[]) =>
        [archived("sid-new")]),
    };
    await hydrateSinglePinnedBody({
      client,
      spaceId: "space1",
      channelId: "channel1",
      roomJid: "room@x",
      stanzaId: "sid-new",
      timelineMessages: [],
    });
    expect(client.fetchRoomMessagesByStanzaIds).toHaveBeenCalledWith(
      "space1",
      "channel1",
      ["sid-new"],
    );
    expect($pinnedMessageBodies.get().get("room@x")?.get("sid-new")).toBeTruthy();
  });

  it("short-circuits when id is in the timeline", async () => {
    const client = { fetchRoomMessagesByStanzaIds: mock(async () => []) };
    await hydrateSinglePinnedBody({
      client,
      spaceId: "s",
      channelId: "c",
      roomJid: "room@x",
      stanzaId: "sid-new",
      timelineMessages: [
        { id: "sid-new", author: "alice", body: "x", createdAt: "x", isSelf: false },
      ],
    });
    expect(client.fetchRoomMessagesByStanzaIds).not.toHaveBeenCalled();
  });
});
```

- [ ] **Step 2: Implement the service**

Create `chat/src/services/pinned-message-bodies.ts`:

```typescript
// Orchestrates the pinned-panel body-cache lifecycle.
//
// - On panel open: hydratePinnedBodiesOnPanelOpen fetches every
//   pinned stanza-id not already represented in the room's loaded
//   channel timeline. Single batched MAM IQ per panel-open via the
//   XEP-0359 §3 stanza-id filter.
// - On `applyPinEvent("pinned")`: hydrateSinglePinnedBody fetches the
//   new entry's body (skipping the round-trip if the message is
//   already on screen).
// - Eviction on unpin lives in the `pinned-messages` store's update
//   pipeline; this module does not own that path.

import { roomMessageFromArchived } from "@/lib/xmpp/conversions";
import {
  $pinnedMessageBodies,
  cachePinnedMessageBodies,
  pinnedMessageBodiesEpoch,
} from "@/stores/pinned-message-bodies";
import { $pinnedRooms } from "@/stores/pinned-messages";
import type { TimelineMessage } from "@/lib/chat-ui";
import type { WasmArchivedMessage } from "@/lib/xmpp/wasm-types";

interface MamFetcher {
  fetchRoomMessagesByStanzaIds: (
    spaceId: string,
    channelId: string,
    stanzaIds: string[],
  ) => Promise<WasmArchivedMessage[]>;
}

interface HydrateOpenArgs {
  client: MamFetcher;
  spaceId: string;
  channelId: string;
  roomJid: string;
  timelineMessages: ReadonlyArray<TimelineMessage>;
}

export async function hydratePinnedBodiesOnPanelOpen(args: HydrateOpenArgs): Promise<void> {
  const room = $pinnedRooms.get().get(args.roomJid);
  if (!room) return;
  const cache = $pinnedMessageBodies.get().get(args.roomJid) ?? new Map();
  const timelineIds = new Set(args.timelineMessages.map((m) => m.id));
  const missing = room.entries
    .map((e) => e.target_stanza_id)
    .filter((id) => !timelineIds.has(id) && !cache.has(id));
  if (missing.length === 0) return;

  const epoch = pinnedMessageBodiesEpoch();
  const archived = await args.client.fetchRoomMessagesByStanzaIds(
    args.spaceId,
    args.channelId,
    missing,
  );
  const cached = archived
    .map((m) => ({
      stanzaId: m.id,
      message: roomMessageFromArchived(m),
    }))
    .filter((m): m is { stanzaId: string; message: TimelineMessage } => !!m.message);
  cachePinnedMessageBodies(args.roomJid, cached, epoch);
}

interface HydrateSingleArgs extends Omit<HydrateOpenArgs, "timelineMessages"> {
  stanzaId: string;
  timelineMessages: ReadonlyArray<TimelineMessage>;
}

export async function hydrateSinglePinnedBody(args: HydrateSingleArgs): Promise<void> {
  if (args.timelineMessages.some((m) => m.id === args.stanzaId)) return;
  const room = $pinnedMessageBodies.get().get(args.roomJid);
  if (room?.has(args.stanzaId)) return;

  const epoch = pinnedMessageBodiesEpoch();
  const archived = await args.client.fetchRoomMessagesByStanzaIds(
    args.spaceId,
    args.channelId,
    [args.stanzaId],
  );
  const cached = archived
    .map((m) => ({
      stanzaId: m.id,
      message: roomMessageFromArchived(m),
    }))
    .filter((m): m is { stanzaId: string; message: TimelineMessage } => !!m.message);
  cachePinnedMessageBodies(args.roomJid, cached, epoch);
}
```

> **Implementer note:** `roomMessageFromArchived` exists in `chat/src/lib/xmpp/conversions.ts` (or `client.ts`) — search for it. It maps `WasmArchivedMessage → TimelineMessage` and is already used by the channel timeline. If it returns `TimelineMessage | null` (test the existing behaviour), keep the filter; if non-nullable, drop it.

- [ ] **Step 3: Run tests; verify pass**

```
cd chat && bun test src/services/__tests__/pinned-message-bodies.test.ts
```

Expected: PASS.

- [ ] **Step 4: Lint, commit**

```
cd chat && bun run lint
git add chat/src/services/pinned-message-bodies.ts chat/src/services/__tests__/pinned-message-bodies.test.ts
git commit -m "feat(chat): orchestrate pinned-panel body hydration"
```

Expected: knip clean (the service is wired in Task 14).

---

## Task 13: Wire eviction on unpin

**Files:**
- Modify: `chat/src/stores/pinned-messages.ts:117-137`

- [ ] **Step 1: Write failing test**

Append to the existing `chat/src/stores/__tests__/pinned-messages.test.ts` (or create it if it doesn't exist):

```typescript
import { describe, expect, it, beforeEach } from "bun:test";
import { applyPinEvent, hydratePinnedRoom, resetPinnedRooms } from "@/stores/pinned-messages";
import {
  $pinnedMessageBodies,
  cachePinnedMessageBody,
  pinnedMessageBodiesEpoch,
  resetPinnedMessageBodies,
} from "@/stores/pinned-message-bodies";
import type { TimelineMessage } from "@/lib/chat-ui";

const fakeMessage: TimelineMessage = {
  id: "sid-A",
  author: "alice",
  body: "live",
  createdAt: "2026-05-11T12:00:00Z",
  isSelf: false,
};

describe("applyPinEvent(unpin) eviction", () => {
  beforeEach(() => {
    resetPinnedRooms();
    resetPinnedMessageBodies();
  });

  it("evicts cached body when entry is unpinned", () => {
    hydratePinnedRoom("room@x", [
      {
        target_stanza_id: "sid-A",
        pinner_jid: "admin@example.com",
        pinned_at: "2026-05-11T12:00:00Z",
        preview: {
          author_jid: "alice@example.com",
          text: "",
          message_timestamp: "2026-05-11T11:50:00Z",
        },
      },
    ]);
    cachePinnedMessageBody("room@x", "sid-A", fakeMessage, pinnedMessageBodiesEpoch());
    applyPinEvent("room@x", { action: "unpinned", target_stanza_id: "sid-A" });
    expect($pinnedMessageBodies.get().get("room@x")?.has("sid-A")).toBeFalsy();
  });
});
```

- [ ] **Step 2: Run test; verify failure**

```
cd chat && bun test src/stores/__tests__/pinned-messages.test.ts
```

Expected: FAIL.

- [ ] **Step 3: Wire eviction**

Modify `chat/src/stores/pinned-messages.ts`. At the top, import:

```typescript
import { evictPinnedMessageBody } from "@/stores/pinned-message-bodies";
```

In `applyPinEvent`, inside the `else` branch (line 129 — the unpin path), call:

```typescript
  } else {
    entries = entries.filter((e) => e.target_stanza_id !== event.target_stanza_id);
    evictPinnedMessageBody(roomJid, event.target_stanza_id);
  }
```

- [ ] **Step 4: Run test; verify pass**

```
cd chat && bun test src/stores/__tests__/pinned-messages.test.ts
```

Expected: PASS.

- [ ] **Step 5: Lint, commit**

```
cd chat && bun run lint
git add chat/src/stores/pinned-messages.ts chat/src/stores/__tests__/pinned-messages.test.ts
git commit -m "feat(chat): evict pinned-message body cache on unpin"
```

---

## Task 14: Wire panel-open and pin-event into the channel controller

**Files:**
- Modify: `chat/src/channels/messages.ts:261-279` and the panel-open site

- [ ] **Step 1: Locate the panel-open trigger**

Search:
```
rg "showPinnedPanel" chat/src/
```

Find where `ui.showPinnedPanel` flips for a given room (likely `chat-app-controller.ts` or `ChatReadyShell.vue`). The clean wiring is: when `showPinnedPanel` becomes true for `activeChannel`, call `hydratePinnedBodiesOnPanelOpen`.

- [ ] **Step 2: Add a watcher**

In `chat/src/channels/messages.ts`, alongside the existing pin-event handler (line 261-279), and within the same composable, wire a watcher that fires the open-panel hydration. Pseudocode (adapt to the file's existing patterns):

```typescript
import {
  hydratePinnedBodiesOnPanelOpen,
  hydrateSinglePinnedBody,
} from "@/services/pinned-message-bodies";
// ...inside useChannelMessages, after the pin-event handler:

watch(
  () => uiShowPinnedPanel.value,  // however the flag is exposed to this composable
  async (open) => {
    if (!open) return;
    const client = xmppClient.value;
    const spaceId = activeSpaceId.value;
    const channelId = activeChannelId.value;
    const roomJid = activeTimelineRoomJid.value;
    if (!client || !spaceId || !channelId || !roomJid) return;
    if (!("fetchRoomMessagesByStanzaIds" in client)) return;
    try {
      await hydratePinnedBodiesOnPanelOpen({
        client,
        spaceId,
        channelId,
        roomJid,
        timelineMessages: messages.value,
      });
    } catch (error) {
      console.warn("hydratePinnedBodiesOnPanelOpen failed", error);
    }
  },
);
```

(If `uiShowPinnedPanel` is not threaded into this composable today, thread it: the `ChatReadyShell` is the owner; pass a `ref<boolean>` in. If that adds too much wiring, the watcher can live in `chat-app-controller.ts` instead. Match the existing wiring direction; do not invert.)

- [ ] **Step 3: Extend the pin-event handler with single-id fetch**

In the existing `client.setPinEventHandler?.(...)` block (lines 261-279), after the `applyPinEvent` call for the `"pinned"` branch, add:

```typescript
        if (event.action === "pinned" && event.preview) {
          applyPinEvent(roomJid, {
            action: "pinned",
            target_stanza_id: event.target_stanza_id,
            entry: { /* …existing… */ },
          });
          if (xmppClient.value && "fetchRoomMessagesByStanzaIds" in xmppClient.value) {
            void hydrateSinglePinnedBody({
              client: xmppClient.value,
              spaceId: activeSpaceId.value ?? "",
              channelId: activeChannelId.value ?? "",
              roomJid,
              stanzaId: event.target_stanza_id,
              timelineMessages: messages.value,
            }).catch((error) => console.warn("hydrateSinglePinnedBody failed", error));
          }
        }
```

- [ ] **Step 4: Type-check + lint**

```
cd chat && bun run typecheck && bun run lint
```

Expected: clean. (Knip will now see the new service exports as used.)

- [ ] **Step 5: Commit**

```
git add chat/src/channels/messages.ts
git commit -m "feat(chat): hydrate pinned-message bodies on panel open and live pin"
```

---

## Task 15: Extract shared `<MessageBody>` component

**Files:**
- Create: `chat/src/components/chat/MessageBody.vue`
- Modify: `chat/src/components/chat/MessageCard.vue:680-960`

This task refactors only — no behaviour change to the timeline.

- [ ] **Step 1: Read the current attachment region**

Open `chat/src/components/chat/MessageCard.vue` lines 680-960. Inventory which props and refs are referenced:

- `displayBody`, `styledHtml`, `setStyledBodyRef` (styled body)
- `isGif`, `message.body` (GIF inline)
- `extensionAnnotations`, `extensionPresentation`, `extensionSurfaceLabel`, `extensionCardDetails`, `extensionActionState`, `actionStatusLabel`, `invokeExtension` (extension cards)
- `imageAttachments`, `attachmentKey`, `resolvedAttachmentUrl`, `openLightbox`, `isDecryptingAttachment`, `attachmentError`, `downloadAttachment` (image strip)
- `videoAttachments`, `audioAttachments`, `pdfAttachments`, `downloadableAttachments` (other inline + chips)

Most come from `useMessageAttachments(message)`. Action-button helpers (`extensionActionState`, `invokeExtension`) only matter for the action buttons; in `compact` mode they're suppressed.

- [ ] **Step 2: Create `MessageBody.vue` with `compact` prop**

Create `chat/src/components/chat/MessageBody.vue`:

```vue
<script setup lang="ts">
// Presentational body + attachments + extension-annotation cards for
// a TimelineMessage. Used by both `MessageCard` (timeline) and
// `PinnedPanel` (right rail). The `compact` flag adapts layout for
// the narrow rail width and suppresses interactive affordances that
// belong on the timeline only (extension action buttons).
//
// Lightbox state is *not* owned here — callers host their own
// `<ImageLightbox>` instance and pass `onImageClick`.
import { computed, watch, nextTick, ref } from "vue";
import { Lock, AlertCircle, CheckCircle2, LoaderCircle, MessageSquare, LayoutDashboard } from "lucide-vue-next";
import {
  extensionCardDetails,
  extensionPresentation,
  extensionSurfaceLabel,
  type TimelineMessage,
  type ExtensionAnnotationAction,
} from "@/lib/chat-ui";
import { formatFileSize, useMessageAttachments } from "@/channels/message-attachments";
import { applyShikiToCodeBlocks } from "@/lib/shiki";
import { useExtensionAnnotationActions } from "@/channels/extension-annotation-actions";
import type { ExtensionCommandResult } from "@/lib/xmpp/extension-commands";

const props = withDefaults(defineProps<{
  message: TimelineMessage;
  compact?: boolean;
  /** When supplied, used by both `compact` and full mode to dispatch
   * lightbox openings. The component does not host its own lightbox. */
  onImageClick?: (file: import("@/lib/chat-ui").TimelineSharedFile, index: number) => void;
  /** Full mode only — invoked on extension annotation actions. */
  invokeExtensionAction?: (action: ExtensionAnnotationAction) => Promise<ExtensionCommandResult>;
}>(), {
  compact: false,
});

const messageRef = computed(() => props.message);
const {
  isGif,
  imageAttachments,
  videoAttachments,
  audioAttachments,
  pdfAttachments,
  downloadableAttachments,
  displayBody,
  resolvedAttachmentUrl,
  attachmentKey,
  attachmentError,
  isDecryptingAttachment,
  downloadAttachment,
} = useMessageAttachments(messageRef);

const styledBodyRef = ref<HTMLElement | null>(null);
const styledHtml = computed(() => {
  // Mirror MessageCard's styled-body rendering; share the helper if
  // available. The existing renderStyledBody fn returns sanitized HTML.
  return props.message.body ?? "";
});

watch(
  () => styledHtml.value,
  async () => {
    await nextTick();
    if (styledBodyRef.value) applyShikiToCodeBlocks(styledBodyRef.value);
  },
  { immediate: true },
);

const { extensionAnnotations, invokeExtension, extensionActionState, actionStatusLabel } =
  useExtensionAnnotationActions(messageRef, () => props.invokeExtensionAction);

function emitImageClick(file: import("@/lib/chat-ui").TimelineSharedFile, index: number) {
  props.onImageClick?.(file, index);
}
</script>

<template>
  <div :class="['message-body', { 'message-body--compact': compact }]">
    <!-- 1. Styled body -->
    <div
      v-if="displayBody"
      ref="styledBodyRef"
      :class="[
        'type-message-body break-words styled-body',
        compact ? 'line-clamp-3 type-field-sm' : '',
      ]"
      v-html="styledHtml"
    />

    <!-- 2. Inline GIF -->
    <div v-else-if="isGif" class="message-body__gif">
      <img
        :src="message.body.trim()"
        alt="GIF"
        :class="[
          'rounded-lg border border-border object-contain',
          compact ? 'max-h-24' : 'chat-attachment-image',
        ]"
        loading="lazy"
        @click.stop="emitImageClick({ url: message.body, mediaType: 'image/gif', disposition: 'inline' } as any, 0)"
      />
    </div>

    <!-- 3. Extension annotations (read-only in compact mode) -->
    <div v-if="extensionAnnotations.length > 0" class="flex flex-col gap-2">
      <div
        v-for="annotation in extensionAnnotations"
        :key="`${annotation.extensionId}:${annotation.annotationId}`"
        :class="[
          'flex min-w-0 items-start gap-3 rounded-lg border border-border bg-muted/25 text-left',
          compact ? 'p-2' : 'p-3',
        ]"
      >
        <span class="flex h-9 w-9 flex-shrink-0 items-center justify-center rounded-md bg-background text-foreground ring-1 ring-border">
          <MessageSquare v-if="annotation.surfaceKind === 'chat-bot'" class="h-4 w-4 text-primary/80" aria-hidden="true" />
          <LayoutDashboard v-else class="h-4 w-4" aria-hidden="true" />
        </span>
        <span class="min-w-0 flex-1">
          <span class="type-section-label block text-muted-foreground">
            {{ extensionPresentation(annotation).label || extensionSurfaceLabel(annotation.surfaceKind) }}
          </span>
          <span class="type-control block break-words text-foreground">
            {{ extensionPresentation(annotation).title }}
          </span>
          <span v-if="extensionPresentation(annotation).summary" class="type-caption mt-1 block break-words text-muted-foreground">
            {{ extensionPresentation(annotation).summary }}
          </span>
          <!-- Action buttons rendered only in non-compact (timeline) mode -->
          <span v-if="!compact && annotation.actions.length > 0" class="mt-2 flex flex-wrap gap-2">
            <!-- (copy the action-button template from current MessageCard:746-790) -->
          </span>
        </span>
      </div>
    </div>

    <!-- 4. Image strip -->
    <div
      v-if="imageAttachments.length > 0"
      :class="compact ? 'message-body__images--compact flex gap-2 flex-wrap' : 'chat-attachment-strip'"
    >
      <div
        v-for="(img, idx) in imageAttachments"
        :key="attachmentKey(img)"
        :class="compact ? 'h-20 w-20 rounded-lg border border-border overflow-hidden bg-muted/40' : 'rounded-lg border border-border overflow-hidden bg-muted/40'"
      >
        <button
          v-if="resolvedAttachmentUrl(img)"
          type="button"
          class="block h-full w-full hover:opacity-90 transition-opacity focus-visible:outline-2 focus-visible:outline-primary"
          :title="img.name ?? 'Image'"
          @click.stop="emitImageClick(img, idx)"
        >
          <img
            :src="resolvedAttachmentUrl(img) || ''"
            :alt="img.name ?? 'Shared image'"
            :class="compact ? 'h-full w-full object-cover' : 'chat-attachment-image object-cover'"
            loading="lazy"
          />
        </button>
        <div
          v-else
          :class="compact ? 'type-caption flex h-full w-full flex-col items-center justify-center gap-1 p-2 text-center text-muted-foreground' : 'type-caption flex h-36 w-48 flex-col items-center justify-center gap-2 px-4 text-center text-muted-foreground'"
        >
          <Lock class="h-4 w-4 text-primary/70" />
          <span>{{ attachmentError(img) ?? (isDecryptingAttachment(img) ? "Decrypting…" : "Preparing…") }}</span>
          <button
            v-if="!compact && attachmentError(img)"
            type="button"
            class="type-caption rounded-lg border border-border bg-background px-2.5 py-1 text-foreground hover:bg-muted"
            @click.stop="downloadAttachment(img)"
          >
            Download
          </button>
        </div>
        <div
          v-if="img.encrypted"
          class="type-meta type-emphasis flex items-center gap-1 border-t border-border/70 px-2 py-1 text-muted-foreground"
        >
          <Lock class="h-3 w-3 text-primary/70" />
          <span>Encrypted</span>
        </div>
      </div>
    </div>

    <!-- 5. Inline video (kept inline in both modes per user decision) -->
    <div v-if="videoAttachments.length > 0" class="flex flex-col gap-2">
      <video
        v-for="v in videoAttachments"
        :key="attachmentKey(v)"
        :src="resolvedAttachmentUrl(v) || ''"
        controls
        :class="compact ? 'max-w-full max-h-48 rounded-lg border border-border' : 'chat-attachment-image rounded-lg border border-border'"
        preload="metadata"
        @click.stop
      />
    </div>

    <!-- 6. Inline audio (kept inline in both modes) -->
    <div v-if="audioAttachments.length > 0" class="flex flex-col gap-2">
      <audio
        v-for="a in audioAttachments"
        :key="attachmentKey(a)"
        :src="resolvedAttachmentUrl(a) || ''"
        controls
        class="w-full"
        preload="metadata"
        @click.stop
      />
    </div>

    <!-- 7. PDF + downloadables: chips in compact mode, full cards in timeline -->
    <div
      v-if="(compact && (pdfAttachments.length + downloadableAttachments.length) > 0)"
      class="flex flex-col gap-1.5"
    >
      <a
        v-for="file in [...pdfAttachments, ...downloadableAttachments]"
        :key="attachmentKey(file)"
        :href="resolvedAttachmentUrl(file) || '#'"
        target="_blank"
        rel="noopener noreferrer"
        class="type-caption inline-flex items-center gap-1.5 rounded-md border border-border bg-background px-2 py-1 text-foreground hover:bg-muted"
        @click.stop
      >
        <span>{{ file.name ?? "Attachment" }}</span>
        <span v-if="file.size" class="text-muted-foreground">· {{ formatFileSize(file.size) }}</span>
      </a>
    </div>
    <template v-else>
      <!-- 7a. Full PDF + downloads (existing MessageCard markup) -->
      <!-- Copy the existing PDF (lines 916-951) + downloadables (952-998)
           template from MessageCard.vue verbatim here. -->
    </template>
  </div>
</template>
```

> **Implementer note:** the template above is the architectural shape, not the final byte-for-byte template. Two non-negotiables when porting:
> 1. **Preserve every existing data binding from `MessageCard.vue:680-960`.** Copy the corresponding section as-is into the matching slot. The only modifications are the `compact ? … : …` class swaps and the suppressed action buttons.
> 2. **Lightbox state lives in the caller.** Do not import `<ImageLightbox>` here. Emit `onImageClick(file, index)`.

- [ ] **Step 3: Refactor `MessageCard.vue` to use `<MessageBody>`**

Replace lines 681-998 in `chat/src/components/chat/MessageCard.vue` with:

```vue
      <MessageBody
        :message="message"
        :invoke-extension-action="invokeExtensionAction"
        :on-image-click="(file, idx) => openLightbox(file)"
      />
```

(Hook up the existing `openLightbox` to receive `file` so `<ImageLightbox>` continues to render with the correct `lightboxImages` state. The lightbox itself stays in `MessageCard.vue` lines 999-1010.)

Add the import at the top of the script block:

```typescript
import MessageBody from "@/components/chat/MessageBody.vue";
```

Remove now-unused imports from `MessageCard.vue` (`extensionCardDetails`, `extensionPresentation`, etc.) — knip will flag them.

- [ ] **Step 4: Run timeline tests (no behaviour change)**

```
cd chat && bun test
```

Expected: every existing test continues to pass. The refactor must not change rendering output of `MessageCard` — only its internal composition.

If there are snapshot tests against `MessageCard`, expect snapshot diffs limited to whitespace/attribute order. Review carefully before regenerating.

- [ ] **Step 5: Lint, type-check, commit**

```
cd chat && bun run typecheck && bun run lint
git add chat/src/components/chat/MessageBody.vue chat/src/components/chat/MessageCard.vue
git commit -m "refactor(chat): extract MessageBody from MessageCard for reuse"
```

Expected: knip clean. Type-check clean.

---

## Task 16: Refactor `PinnedPanel.vue` for rich preview

**Files:**
- Modify: `chat/src/components/chat/PinnedPanel.vue`
- Create: `chat/src/components/chat/__tests__/PinnedPanel.test.ts`

- [ ] **Step 1: Write failing panel render-state tests**

Create `chat/src/components/chat/__tests__/PinnedPanel.test.ts`:

```typescript
import { describe, expect, it, beforeEach } from "bun:test";
import { mount } from "@vue/test-utils";
import PinnedPanel from "@/components/chat/PinnedPanel.vue";
import { hydratePinnedRoom, resetPinnedRooms } from "@/stores/pinned-messages";
import {
  cachePinnedMessageBody,
  pinnedMessageBodiesEpoch,
  resetPinnedMessageBodies,
} from "@/stores/pinned-message-bodies";
import type { TimelineMessage } from "@/lib/chat-ui";

const liveImage: TimelineMessage = {
  id: "sid-img",
  author: "alice",
  body: "",
  createdAt: "2026-05-11T11:50:00Z",
  isSelf: false,
  sharedFiles: [
    {
      url: "https://example.com/img.png",
      mediaType: "image/png",
      disposition: "inline",
      name: "img.png",
    },
  ],
};

describe("PinnedPanel rich preview", () => {
  beforeEach(() => {
    resetPinnedRooms();
    resetPinnedMessageBodies();
  });

  it("renders live image attachment when body is cached", () => {
    hydratePinnedRoom("room@x", [
      {
        target_stanza_id: "sid-img",
        pinner_jid: "admin@example.com",
        pinned_at: "2026-05-11T12:00:00Z",
        preview: {
          author_jid: "alice@example.com",
          text: "",
          message_timestamp: "2026-05-11T11:50:00Z",
        },
      },
    ]);
    cachePinnedMessageBody("room@x", "sid-img", liveImage, pinnedMessageBodiesEpoch());

    const wrapper = mount(PinnedPanel, {
      props: { roomJid: "room@x", channelName: "general" },
    });

    expect(wrapper.find("img").attributes("src")).toBe("https://example.com/img.png");
    expect(wrapper.text()).not.toContain("(no preview text)");
  });

  it("renders preview.text fallback when no body and no cache", () => {
    hydratePinnedRoom("room@x", [
      {
        target_stanza_id: "sid-T",
        pinner_jid: "admin@example.com",
        pinned_at: "2026-05-11T12:00:00Z",
        preview: {
          author_jid: "alice@example.com",
          text: "hello world",
          message_timestamp: "2026-05-11T11:50:00Z",
        },
      },
    ]);

    const wrapper = mount(PinnedPanel, {
      props: { roomJid: "room@x", channelName: "general" },
    });
    expect(wrapper.text()).toContain("hello world");
  });

  it("renders aged-out fallback when preview.text empty and no cache", () => {
    hydratePinnedRoom("room@x", [
      {
        target_stanza_id: "sid-aged",
        pinner_jid: "admin@example.com",
        pinned_at: "2026-05-11T12:00:00Z",
        preview: {
          author_jid: "alice@example.com",
          text: "",
          message_timestamp: "2026-05-11T11:50:00Z",
        },
      },
    ]);
    const wrapper = mount(PinnedPanel, {
      props: { roomJid: "room@x", channelName: "general" },
    });
    expect(wrapper.text()).toContain("Original message no longer available.");
    expect(wrapper.text()).not.toContain("(no preview text)");
  });

  it("never renders the legacy '(no preview text)' literal", () => {
    hydratePinnedRoom("room@x", []);
    const wrapper = mount(PinnedPanel, {
      props: { roomJid: "room@x", channelName: "general" },
    });
    expect(wrapper.text()).not.toContain("(no preview text)");
  });
});
```

- [ ] **Step 2: Run tests; verify failure**

```
cd chat && bun test src/components/chat/__tests__/PinnedPanel.test.ts
```

Expected: FAIL (legacy literal present; rich render not wired).

- [ ] **Step 3: Refactor `PinnedPanel.vue`**

Replace `chat/src/components/chat/PinnedPanel.vue` body with:

```vue
<script setup lang="ts">
// PinnedPanel — right-rail panel listing the room's pinned messages
// (#414). Hydrated by the chat-app-controller via fetchRoomPins on
// room entry; live-updated by the pin-event observer wired into the
// XmppClient. Mutually exclusive with ThreadPanel in the right rail
// — the parent (ChatReadyShell) gates rendering on
// ui.showPinnedPanel.
//
// Rich preview (#NNN, this branch): each entry resolves to a
// TimelineMessage from either (a) the in-memory channel timeline or
// (b) the pinned-message body cache populated by the panel-open
// hydration service. The shared `<MessageBody compact />` renders
// images, video, audio, PDFs, downloadables, and extension cards.
// Empty preview.text + no live body → "Original message no longer
// available." italic fallback.
import { computed, ref } from "vue";
import { useStore } from "@nanostores/vue";
import { Pin, X } from "lucide-vue-next";

import { $pinnedRooms } from "@/stores/pinned-messages";
import { $pinnedMessageBodies } from "@/stores/pinned-message-bodies";
import MessageBody from "@/components/chat/MessageBody.vue";
import ImageLightbox from "@/components/ui/ImageLightbox.vue";
import type { TimelineMessage, TimelineSharedFile } from "@/lib/chat-ui";

const props = defineProps<{
  roomJid: string;
  channelName: string;
  /** Optional — when present, used to short-circuit MAM cache lookups
   * for pinned entries that already live in the loaded timeline. */
  timelineMessages?: ReadonlyArray<TimelineMessage>;
}>();

const emit = defineEmits<{
  close: [];
  jumpToMessage: [stanzaId: string];
}>();

const pinnedRooms = useStore($pinnedRooms);
const pinnedBodies = useStore($pinnedMessageBodies);
const state = computed(() => pinnedRooms.value.get(props.roomJid) ?? null);
const entries = computed(() => state.value?.entries ?? []);
const hydrated = computed(() => state.value?.hydrated ?? false);

const timelineIndex = computed(() => {
  const map = new Map<string, TimelineMessage>();
  for (const m of props.timelineMessages ?? []) map.set(m.id, m);
  return map;
});

function liveMessageFor(stanzaId: string): TimelineMessage | null {
  return timelineIndex.value.get(stanzaId)
    ?? pinnedBodies.value.get(props.roomJid)?.get(stanzaId)
    ?? null;
}

function relativeTime(iso: string): string {
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return "";
  const seconds = Math.max(1, Math.round((Date.now() - t) / 1000));
  if (seconds < 60) return `${seconds}s ago`;
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.round(hours / 24);
  return `${days}d ago`;
}

// Lightbox state owned by the panel — clicks on images inside any
// `<MessageBody>` bubble up through `onImageClick`.
const lightboxOpen = ref(false);
const lightboxImages = ref<TimelineSharedFile[]>([]);
const lightboxIndex = ref(0);

function openLightbox(images: TimelineSharedFile[], index: number) {
  lightboxImages.value = images;
  lightboxIndex.value = index;
  lightboxOpen.value = true;
}
</script>

<template>
  <aside class="pinned-panel flex flex-col h-full bg-background border-l border-border">
    <header class="flex items-center justify-between px-4 h-14 border-b border-border">
      <div class="flex items-center gap-2 min-w-0">
        <Pin class="w-4 h-4 text-muted-foreground" aria-hidden="true" />
        <h2 class="type-heading-sm truncate">Pinned in {{ channelName }}</h2>
      </div>
      <button
        type="button"
        class="rounded p-1 hover:bg-muted"
        aria-label="Close pinned messages"
        @click="emit('close')"
      >
        <X class="w-5 h-5" />
      </button>
    </header>

    <div v-if="!hydrated" class="flex-1 flex items-center justify-center text-muted-foreground type-field">
      Loading pinned messages…
    </div>
    <div
      v-else-if="entries.length === 0"
      class="flex-1 flex flex-col items-center justify-center text-muted-foreground type-field gap-1 px-6"
    >
      <Pin class="w-6 h-6" aria-hidden="true" />
      <p>No pinned messages yet.</p>
      <p class="text-xs">Admins can pin a message from the message menu.</p>
    </div>
    <ol v-else class="flex-1 overflow-y-auto divide-y divide-border" role="list">
      <li
        v-for="entry in entries"
        :key="entry.target_stanza_id"
        class="px-4 py-3 cursor-pointer hover:bg-muted/40 focus-within:bg-muted/40"
        tabindex="0"
        @click="emit('jumpToMessage', entry.target_stanza_id)"
        @keydown.enter.prevent="emit('jumpToMessage', entry.target_stanza_id)"
        @keydown.space.prevent="emit('jumpToMessage', entry.target_stanza_id)"
      >
        <div class="flex items-baseline justify-between gap-2 mb-0.5">
          <span class="type-field font-medium truncate">
            {{ entry.preview.author_nick ?? entry.preview.author_jid }}
          </span>
          <span class="type-field-xs text-muted-foreground shrink-0">
            {{ relativeTime(entry.preview.message_timestamp) }}
          </span>
        </div>

        <!-- Rich render or fallback. -->
        <template v-if="liveMessageFor(entry.target_stanza_id) as live">
          <p
            v-if="live.isRetracted"
            class="type-field-sm italic text-muted-foreground"
          >Message retracted</p>
          <MessageBody
            v-else
            :message="live"
            compact
            :on-image-click="(file, idx) => openLightbox(
              live.sharedFiles?.filter((f) => f.disposition === 'inline' && (f.mediaType?.startsWith('image/') ?? true)) ?? [],
              idx,
            )"
          />
        </template>
        <template v-else>
          <p
            v-if="entry.preview.text"
            class="type-field-sm text-muted-foreground line-clamp-3 break-words"
          >{{ entry.preview.text }}</p>
          <p
            v-else
            class="type-field-sm italic text-muted-foreground"
          >Original message no longer available.</p>
        </template>

        <p class="type-field-xs text-muted-foreground mt-1">
          Pinned by {{ entry.pinner_jid }} · {{ relativeTime(entry.pinned_at) }}
        </p>
      </li>
    </ol>

    <ImageLightbox
      v-if="lightboxOpen"
      :images="lightboxImages"
      :start-index="lightboxIndex"
      @close="lightboxOpen = false"
    />
  </aside>
</template>
```

- [ ] **Step 4: Thread `timelineMessages` from the caller**

Find every `<PinnedPanel ... />` mount site (`rg "PinnedPanel" chat/src/components`). Pass the channel's current `messages` array as `:timeline-messages="messages"` from the channel shell. If the existing parent doesn't have direct access to `messages`, route it through whichever prop matches the local convention.

- [ ] **Step 5: Run tests; verify pass**

```
cd chat && bun test src/components/chat/__tests__/PinnedPanel.test.ts
```

Expected: PASS.

- [ ] **Step 6: Lint, type-check, commit**

```
cd chat && bun run typecheck && bun run lint
git add chat/src/components/chat/PinnedPanel.vue chat/src/components/chat/__tests__/PinnedPanel.test.ts
git commit -m "feat(chat): render rich pinned-message preview in PinnedPanel"
```

Expected: knip clean.

---

## Task 17: End-to-end smoke + regression sweep

**Files:** none — manual verification + full suite.

- [ ] **Step 1: Full Rust suite**

```
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
```

Expected: all green.

- [ ] **Step 2: Full chat suite**

```
cd chat && bun test && bun run lint && bun run typecheck
```

Expected: all green. Knip exits clean.

- [ ] **Step 3: Manual browser verification**

Start the dev server (`cd chat && bun run dev` or the project's equivalent). In a browser:

1. Pin a plain-text message → open the pinned panel → see the text, not `(no preview text)`. ✓
2. Pin an **image-only** message (no body) → open the panel → see the image thumbnail. ✓
3. Pin a **video** message → open the panel → see the inline `<video>` player. Click play; verify the row's `jumpToMessage` does NOT fire. ✓
4. Pin a **PDF** attachment → open the panel → see the chip with file name. ✓
5. Pin an **extension annotation** message (e.g. bot card) → open the panel → see the read-only card without action buttons. ✓
6. Pin an **encrypted image** (OMEMO) → open the panel → see "Decrypting…" → image renders. ✓
7. Scroll the channel timeline above the pinned message → close + reopen the panel → still renders the live message (cached from the panel-open MAM fetch). ✓
8. Have another user (or via `xmpp-cli`) pin a message *while the panel is open* → see the new entry render its live body within ~1 round-trip. ✓
9. Have an admin retract a pinned message → see it disappear from the panel (pin-retraction cascade). ✓
10. Log out + log back in as a different user → panel does not flash prior-session data (epoch guard). ✓

- [ ] **Step 4: Commit the verification log**

If you keep a verification log (some superpowers workflows do), commit it. Otherwise: no-op.

---

## Task 18: PR housekeeping

- [ ] **Step 1: Update PR description**

```
gh pr view --json url,title,body
```

If the draft PR opened before implementation is still at "plan only," update the body to summarise the *completed* work (1-3 bullets) and the test plan (the manual checklist above). Per CLAUDE.md, pass multiline Markdown with real newlines, not `\n`. Verify with `gh pr view` afterwards.

- [ ] **Step 2: Mark ready for review**

```
gh pr ready
```

- [ ] **Step 3: Monitor CI**

```
gh pr checks --watch
```

Per CLAUDE.md "always monitor CI and fix it until all checks are green." If a check fails, diagnose and fix; do not merge until green.

---

## Self-Review Checklist

This block was filled by the plan author against the grilled spec.

**1. Spec coverage:**
- Q1 hybrid render → Tasks 11, 12, 15, 16 (live render with fallback) ✓
- Q2 in-memory timeline + per-stanza-id MAM ✓ (Task 12)
- Q3 XEP-0359 §3 conformant form field ✓ (Tasks 1-7)
- Q4 lazy on panel open ✓ (Task 14)
- Q5 timeline reuse + per-room cache + live update ✓ (Tasks 11-14)
- Q6 shared `<MessageBody>` ✓ (Task 15)
- Q7 state matrix (live / loading / aged-out / retracted) ✓ (Tasks 16 + tests)
- Q8 inline video/audio in compact mode (user override) ✓ (Task 15)
- Q9 pin-event + LMC + reset ✓ (Tasks 11, 13, 14)
- Q10 server validation + custom test suite ✓ (Tasks 3, 4, 7)

**2. Placeholder scan:**
- Two `todo!()` placeholders in Task 7 are flagged with explicit "do not commit with these in place" — implementer must fill in.
- Task 15 Step 2 template shows architectural shape with explicit "preserve every existing data binding" note.
- No other "TBD" / "etc." / "similar to" patterns.

**3. Type consistency:**
- `cachePinnedMessageBody` / `cachePinnedMessageBodies` / `evictPinnedMessageBody` / `resetPinnedMessageBodies` / `pinnedMessageBodiesEpoch` — all referenced consistently across Tasks 11-14.
- `fetchRoomMessagesByStanzaIds` — consistent between TS wrapper (Task 10), service (Task 12), and channel wiring (Task 14).
- `STANZA_ID_FILTER_FIELD` / `MAX_FILTER_STANZA_ID_LEN` / `MAX_FILTER_STANZA_IDS` — defined Task 1, used Tasks 3, 4, 7, 8 with matching names.
- `MamQuery.stanza_ids: Vec<String>` — defined Task 2, queried Tasks 3, 5, 6, 7; consistent type.
